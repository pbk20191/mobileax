package dev.mobileax.daemon

import android.accessibilityservice.AccessibilityServiceInfo
import android.app.UiAutomation
import android.content.Context
import android.hardware.display.DisplayManager
import kotlinx.coroutines.launch

/**
 * The handlers behind every verb, once the entry point (Main for
 * app_process, MobaxInstrumentation for `am instrument`) has a Context and
 * a connected UiAutomation. Runs on a Work thread.
 */
internal suspend fun buildServer(context: Context, ua: UiAutomation): Server {
    enableInteractiveWindows(ua)
    val h = Server.Handlers()
    h.dumper = Dumper(ua)
    h.shot = Screenshot(ua).also { it.probeMirror() }
    h.input = Input(ua)
    h.ctx = context
    h.installer = Installer(context)
    h.pm = Pm(context, ua)
    h.manifest = Manifest(context)
    h.providers = Providers()
    h.location = Location()
    h.notifs = Notifications()
    h.clip = Clipboard()
    h.am = Am(context)
    try { h.props = Props() } catch (t: Throwable) { Logx.bootWarn("Props init failed", t) }
    h.settings = SettingsApi(context)
    h.wm = Wm(context.getSystemService(DisplayManager::class.java), ua)
    h.uiEvents = UiEvents(ua)
    h.by = ByEngine(ua, context, h.uiEvents)
    h.xpath = XPathEngine(ua, h.uiEvents)
    h.nodes = NodeActions(ua, h.by)
    h.toasts = Toasts(h.uiEvents)
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
    return Server(h)
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
