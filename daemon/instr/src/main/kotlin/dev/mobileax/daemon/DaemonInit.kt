package dev.mobileax.daemon

import android.app.ActivityManager
import android.app.UiAutomation
import android.os.IInterface
import android.content.Context
import kotlin.system.exitProcess

/**
 * Builds the handlers once the instrumentation is up ([MobaxInstrumentation]).
 *
 * Runs on a Work thread: connecting UiAutomation blocks until its
 * service-connected callback arrives on the main looper, so it must not
 * run there.
 */
internal object DaemonInit {

    /** Runs on a Work thread: connecting UiAutomation blocks until its callback arrives on main. */
    fun connect(): Pair<Context, UiAutomation> {
        val ua = try {
            dev.mobileax.daemon.ua   // ByEngine's UiDevice asks for it with the same UA_FLAGS
        } catch (t: Throwable) {
            throw IllegalStateException(
                "UiAutomation unavailable: ${t.message} — another UiAutomation (e.g. a UiAutomator test) may be connected", t
            )
        }
        exitWithConnection(ua)
        ua.adoptShellPermissionIdentity()
        runAsMonkey(ua)
        return InstrHost.context to ua
    }

    /**
     * The UiAutomation connection lives in the `am instrument` process. If
     * that goes (killed, adb gone) we'd stay up answering ping with nothing
     * behind it, so the host would never restart us: leave with it.
     */
    private fun exitWithConnection(ua: UiAutomation) {
        try {
            val conn = UiAutomation::class.java.getDeclaredField("mUiAutomationConnection")
                .apply { isAccessible = true }.get(ua) as IInterface
            conn.asBinder().linkToDeath({
                Logx.boot("mobaxd UiAutomation connection gone (am instrument ended); exiting")
                exitProcess(0)
            }, 0)
        } catch (t: Throwable) {
            Logx.bootWarn("could not watch the UiAutomation connection", t)
        }
    }

    /** `before` is what a previous daemon left behind: AMS clears the flag
     *  when an instrumentation's UiAutomation goes away, so it reads false. */
    private fun runAsMonkey(ua: UiAutomation) {
        val before = ActivityManager.isUserAMonkey()
        try { ua.setRunAsMonkey(true) } catch (t: Throwable) { Logx.bootWarn("setRunAsMonkey failed", t) }
        Logx.boot("mobaxd monkey before=$before after=${ActivityManager.isUserAMonkey()}")
    }
}
