package dev.mobileax.daemon

import android.app.Instrumentation
import android.app.UiAutomation
import android.content.Context
import android.graphics.Rect
import androidx.test.uiautomator.By
import androidx.test.uiautomator.BySelector
import androidx.test.uiautomator.Configurator
import androidx.test.uiautomator.UiDevice
import java.util.regex.Pattern

/**
 * STUDY (study/uidevice-selector): can the official androidx.test.uiautomator
 * run inside the app_process daemon, with no real instrumentation? UiDevice
 * gets a shim Instrumentation that hands it our own UiAutomation and the
 * system context. Wire: `by_find KEY=VALUE` (one predicate) → count, first
 * bounds, timings. Not for merge.
 */
internal class ByStudy(private val ua: UiAutomation, private val ctx: Context) {

    private class Shim(private val ua: UiAutomation, private val ctx: Context) : Instrumentation() {
        override fun getUiAutomation(): UiAutomation = ua
        override fun getUiAutomation(flags: Int): UiAutomation = ua
        override fun getContext(): Context = ctx
        override fun getTargetContext(): Context = ctx
    }

    private var initNs = 0L
    private val device: UiDevice by lazy {
        val t0 = System.nanoTime()
        Configurator.getInstance().uiAutomationFlags = UiAutomation.FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES
        UiDevice.getInstance(Shim(ua, ctx)).also { initNs = System.nanoTime() - t0 }
    }

    fun find(cmd: String): ByteArray {
        val arg = cmd.substringAfter(' ', "").trim()
        val key = arg.substringBefore('=')
        val v = arg.substringAfter('=', "")
        return try {
            val sel: BySelector = when (key) {
                "text" -> By.text(v)
                "textContains" -> By.textContains(v)
                "textRegexI" -> By.text(Pattern.compile(v, Pattern.CASE_INSENSITIVE))
                "desc" -> By.desc(v)
                "descContains" -> By.descContains(v)
                "res" -> By.res(v)
                "clazz" -> By.clazz(v)
                "clickable" -> By.clickable(true)
                "hasDescendantText" -> By.hasDescendant(By.text(v))
                else -> return "ERR:BAD_ARG:by_find key=$key".toByteArray()
            }
            val d = device
            val t0 = System.nanoTime()
            val found = d.findObjects(sel)
            val findMs = (System.nanoTime() - t0) / 1_000_000.0
            val r = Rect()
            val first = found.firstOrNull()?.let { it.visibleBounds.also { b -> r.set(b) }; "[${r.left},${r.top},${r.right},${r.bottom}]" } ?: "-"
            "ok n=${found.size} first=$first find_ms=${"%.1f".format(findMs)} init_ms=${initNs / 1_000_000}".toByteArray()
        } catch (t: Throwable) {
            "ERR:INTERNAL:by_find:${t.javaClass.simpleName}:${t.message}".toByteArray()
        }
    }
}
