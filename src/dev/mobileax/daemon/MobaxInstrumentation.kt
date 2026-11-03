package dev.mobileax.daemon

import android.os.Bundle
import android.os.StrictMode
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.runner.MonitoringInstrumentation
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch

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
        val port = InstrumentationRegistry.getArguments().getString("port")?.toIntOrNull() ?: 9008
        // ActivityThread turns on StrictMode's network-on-main check for app
        // processes. Every socket here is a non-blocking local one driven by
        // the main looper, so that check would only kill working sends.
        StrictMode.setThreadPolicy(StrictMode.ThreadPolicy.LAX)
        try {
            Loop.init()
            val ready = CompletableDeferred<Server>()
            lateinit var server: MobaxServer
            server = MobaxServer(port, Dispatch(ready, onQuit = {
                server.close()
                finish(0, Bundle())
            }))
            server.start()
            Loop.scope.launch {
                try {
                    ready.complete(DaemonInit.init())
                    Logx.boot("mobaxd ready (sdk=${android.os.Build.VERSION.SDK_INT})")
                } catch (t: Throwable) {
                    Logx.fatal("init failed", t)
                    finish(2, Bundle().apply { putString(Logx.STATUS_KEY, "fatal: init failed: $t") })
                }
            }
        } catch (t: Throwable) {
            Logx.fatal("startup failed", t)
            finish(2, Bundle().apply { putString(Logx.STATUS_KEY, "fatal: startup failed: $t") })
        }
    }

    override fun onDestroy() {
        super.onDestroy()
    }

    override fun onException(obj: Any?, e: Throwable?): Boolean {
        return super.onException(obj, e)
    }
}
