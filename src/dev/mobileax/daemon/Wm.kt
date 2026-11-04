package dev.mobileax.daemon

import android.graphics.Point
import android.hardware.display.DisplayManager
import android.util.DisplayMetrics
import android.view.Display
import java.nio.charset.StandardCharsets

/**
 * window-manager helpers — direct binder against IWindowManager. Replaces the
 * `adb shell wm size|density|user-rotation` pattern which each spawns
 * a fresh `wm` app_process.
 */
internal class Wm(private val dm: DisplayManager, private val ua: android.app.UiAutomation) {
    // Display.getRealSize/getRealMetrics: WindowMetrics needs API 30 and a window context.
    @Suppress("DEPRECATION")
    fun info(): ByteArray {
        try {
            val d = dm.getDisplay(Display.DEFAULT_DISPLAY)
            val size = Point()
            d.getRealSize(size)
            val dmx = DisplayMetrics()
            d.getRealMetrics(dmx)
            val rot = d.rotation
            val json = ("{\"width\":" + size.x
                    + ",\"height\":" + size.y
                    + ",\"density\":" + dmx.densityDpi
                    + ",\"xdpi\":" + dmx.xdpi
                    + ",\"ydpi\":" + dmx.ydpi
                    + ",\"rotation\":" + rot
                    + "}")
            return json.toByteArray(StandardCharsets.UTF_8)
        } catch (t: Throwable) {
            return err("wm-info-failed:" + t.message)
        }
    }

    /** Pins the default display to [rotation] (0..3, Surface.ROTATION_*), through UiAutomation.setRotation. */
    fun setRotation(rotation: Int): ByteArray {
        if (rotation !in 0..3) return err("bad-rotation:$rotation")
        return try {
            // ROTATION_FREEZE_0..270 are 0..3, the same numbers.
            if (ua.setRotation(rotation)) ok("ok rotation=$rotation") else err("rotation-failed:refused")
        } catch (t: Throwable) {
            err("rotation-threw:" + t.javaClass.getSimpleName() + ":" + t.message)
        }
    }

    companion object {
        private fun ok(s: String): ByteArray {
            return s.toByteArray(StandardCharsets.UTF_8)
        }

        private fun err(tail: String?): ByteArray {
            return ("ERR:$tail").toByteArray(StandardCharsets.UTF_8)
        }
    }
}
