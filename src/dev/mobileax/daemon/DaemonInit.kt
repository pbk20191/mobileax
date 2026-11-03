package dev.mobileax.daemon

import android.accessibilityservice.AccessibilityServiceInfo
import android.app.ActivityManager
import android.app.UiAutomation
import android.hardware.display.DisplayManager
import android.os.IInterface
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlin.system.exitProcess

/**
 * Builds the handlers once the instrumentation is up ([MobaxInstrumentation]).
 *
 * Runs on a Work thread: connecting UiAutomation blocks until its
 * service-connected callback arrives on the main looper, so it must not
 * run there.
 */
internal object DaemonInit {

    suspend fun init(): Server = withContext(Loop.Work) {
        val ua = try {
            dev.mobileax.daemon.ua.also { device }   // UiDevice shares it: same UA_FLAGS
        } catch (t: Throwable) {
            throw IllegalStateException(
                "UiAutomation unavailable: ${t.message} — another UiAutomation (e.g. a UiAutomator test) may be connected", t
            )
        }
        exitWithConnection(ua)
        ua.adoptShellPermissionIdentity()
        runAsMonkey(ua)
        enableInteractiveWindows(ua)

        val context = ctx
        val h = Server.Handlers()
        h.dumper = Dumper(ua)
        h.shot = Screenshot(ua).also { it.probeMirror() }
        h.input = Input(ua)
        h.ctx = context
        h.installer = Installer(context)
        h.pm = Pm(context)
        h.manifest = Manifest(context)
        h.providers = Providers()
        h.location = Location()
        h.notifs = Notifications()
        h.clip = Clipboard()
        h.am = Am(context)
        try { h.props = Props() } catch (t: Throwable) { Logx.bootWarn("Props init failed", t) }
        h.settings = SettingsApi(context)
        h.wm = Wm(context.getSystemService(DisplayManager::class.java))
        h.uiEvents = UiEvents(ua)
        h.nodes = NodeActions(ua)
        h.state = State(context, h.uiEvents)

        // Warm the dump path: the first dump_active on a cold VM costs ~1 s.
        Loop.scope.launch(Loop.Work) {
            try {
                val t0 = System.nanoTime()
                h.dumper.dumpActive()
                Logx.i("warmup: dump_active in ${(System.nanoTime() - t0) / 1_000_000}ms")
            } catch (t: Throwable) {
                Logx.w("dump_active warmup failed", t)
            }
        }
        Server(h)
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

    private fun enableInteractiveWindows(ua: UiAutomation) {
        try {
            val info = ua.serviceInfo ?: return
            info.flags = info.flags or AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS
            ua.serviceInfo = info
        } catch (t: Throwable) {
            Logx.bootWarn("enableInteractiveWindows failed", t)
        }
    }
}
