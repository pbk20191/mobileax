package dev.mobileax.daemon

import android.os.Bundle
import android.os.StrictMode
import kotlin.system.exitProcess

/**
 * Starts the daemon from MobaxInstrumentation.onCreate (the androidTest
 * APK), after MonitoringInstrumentation has registered the instrumentation.
 *
 * AMS forks the process, so it runs as a registered instrumentation: the
 * framework hands us UiAutomation, and setRunAsMonkey is allowed. onCreate
 * runs on the main thread, whose looper ActivityThread already runs; the
 * server and every coroutine live there (see [Loop]). The instrumentation
 * lasts until `quit`, `am force-stop`, the `am instrument` process going
 * away, or no client for a while on a physical device ([Lease]).
 *
 * Kept by name in proguard-rules.pro: nothing in this APK calls it.
 */
fun startInstrumentation(arguments: Bundle) {
    Host.current = InstrHost
    val port = arguments.getString("port")?.toIntOrNull() ?: 9008
    UaShell.forceFiles = arguments.getString("shell") == "file"
    // ActivityThread turns on StrictMode's network-on-main check for app
    // processes. Every socket here is a non-blocking local one driven by
    // the main looper, so that check would only kill working sends.
    StrictMode.setThreadPolicy(StrictMode.ThreadPolicy.LAX)
    try {
        Loop.init()
        serve(port, connect = { DaemonInit.connect() }, onInitFailed = { giveUp() })
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
