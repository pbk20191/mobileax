package dev.mobileax.daemon

import android.os.Bundle
import android.os.StrictMode
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.runner.MonitoringInstrumentation
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import kotlin.system.exitProcess

/**
 * Entry point: `am instrument -w --no-hidden-api-checks -e port N
 * dev.mobileax.daemon/.MobaxInstrumentation`.
 *
 * AMS forks the process, so it runs as a registered instrumentation: the
 * framework hands us UiAutomation, and setRunAsMonkey is allowed. onCreate
 * runs on the main thread, whose looper ActivityThread already runs; the
 * server and every coroutine live there (see [Loop]). start() is never
 * called: there are no tests, and the instrumentation lasts until `quit`,
 * `am force-stop` or the `am instrument` process going away.
 */
class MobaxInstrumentation : MonitoringInstrumentation() {

    override fun onCreate(arguments: Bundle) {
        super.onCreate(arguments)
        val args = InstrumentationRegistry.getArguments()
        val port = args.getString("port")?.toIntOrNull() ?: 9008
        Shell.forceFiles = args.getString("shell") == "file"
        // ActivityThread turns on StrictMode's network-on-main check for app
        // processes. Every socket here is a non-blocking local one driven by
        // the main looper, so that check would only kill working sends.
        StrictMode.setThreadPolicy(StrictMode.ThreadPolicy.LAX)
        try {
            Loop.init()
            val ready = CompletableDeferred<Server>()
            lateinit var server: MobaxServer
            // `quit` exits the process right after (Dispatch); finish() could block here.
            server = MobaxServer(port, Dispatch(ready, onQuit = { server.close() }))
            server.start()
            Loop.scope.launch {
                try {
                    ready.complete(DaemonInit.init())
                    Logx.boot("mobaxd ready (sdk=${android.os.Build.VERSION.SDK_INT})")
                } catch (t: Throwable) {
                    Logx.fatal("init failed", t)
                    ready.completeExceptionally(t)   // `ready` callers get the failure now
                    giveUp()
                }
            }
        } catch (t: Throwable) {
            Logx.fatal("startup failed", t)
            giveUp()
        }
    }

    /**
     * Ends the process after a failed start. Not finish(): MonitoringInstrumentation
     * waits there for activities to finish and never returns. AMS then
     * reports the instrumentation as crashed, which ends `am instrument`
     * (and its log) too. The pause lets the fatal status line reach the log.
     */
    private fun giveUp() {
        Thread({ Thread.sleep(300); exitProcess(2) }, "mobax-exit").start()
    }

    override fun onDestroy() {
        super.onDestroy()
    }

    override fun onException(obj: Any?, e: Throwable?): Boolean {
        return super.onException(obj, e)
    }
}
