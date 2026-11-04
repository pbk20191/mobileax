package dev.mobileax.daemon

import android.graphics.Point
import android.graphics.Rect
import android.hardware.display.DisplayManager
import android.hardware.display.DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR
import android.os.Build
import android.os.IBinder
import android.view.Display
import android.view.Surface

/**
 * A display that mirrors the default one onto [Surface]: what screenshots
 * (Screenshot.Mirror) and the h264 stream draw from.
 *
 * Where the shell may make one (SDK 34+, which grants it
 * CAPTURE_VIDEO_OUTPUT) it is DisplayManager's auto-mirror virtual display.
 * Below that DisplayManager refuses, so the mirror is made straight in
 * SurfaceFlinger with the hidden SurfaceControl display API, as scrcpy does
 * from app_process: no permission dialog, no notification.
 *
 * A SurfaceControl mirror keeps the orientation it was made in; [rotation]
 * lets the owner make a new one when the display turns.
 */
internal class MirrorDisplay private constructor(
    private val release: () -> Unit,
    /** The default display's rotation when this was made. */
    val rotation: Int,
    /** Made through SurfaceControl (SDK < 34). */
    val viaSurfaceControl: Boolean,
) : AutoCloseable {

    override fun close() {
        try { release() } catch (_: Throwable) {}
    }

    companion object {
        fun currentRotation(dm: DisplayManager): Int = dm.getDisplay(Display.DEFAULT_DISPLAY)?.rotation ?: 0

        /** The default display's size as it is turned now. */
        fun currentSize(dm: DisplayManager): Point {
            val p = Point()
            @Suppress("DEPRECATION") // WindowMetrics needs API 30 and a window context
            dm.getDisplay(Display.DEFAULT_DISPLAY)?.getRealSize(p)
            return p
        }

        fun open(dm: DisplayManager, name: String, outW: Int, outH: Int, surface: Surface): MirrorDisplay {
            val rotation = currentRotation(dm)
            try {
                val vd = dm.createVirtualDisplay(name, outW, outH, 320, surface, VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR)
                    ?: throw RuntimeException("createVirtualDisplay returned null")
                return MirrorDisplay({ vd.release() }, rotation, false)
            } catch (e: SecurityException) {
                // Only the missing CAPTURE_VIDEO_OUTPUT below 14 has a way round.
                if (Build.VERSION.SDK_INT >= 34) throw e
                Logx.i("mirror: virtual display refused (${e.message}); through SurfaceControl")
            }
            val src = currentSize(dm)
            val token = SurfaceDisplay.create(name, surface, Rect(0, 0, src.x, src.y), Rect(0, 0, outW, outH))
            return MirrorDisplay({ SurfaceDisplay.destroy(token) }, rotation, true)
        }
    }
}

/** The hidden `android.view.SurfaceControl` display calls, by reflection. */
private object SurfaceDisplay {
    private val sc by lazy { Class.forName("android.view.SurfaceControl") }

    /** The default display's layer stack (0 unless the device says otherwise). */
    private fun layerStack(): Int = try {
        val g = Class.forName("android.hardware.display.DisplayManagerGlobal")
        val inst = g.getMethod("getInstance").invoke(null)
        val info = g.getMethod("getDisplayInfo", Int::class.javaPrimitiveType).invoke(inst, Display.DEFAULT_DISPLAY)
        info.javaClass.getField("layerStack").getInt(info)
    } catch (_: Throwable) {
        0
    }

    fun create(name: String, surface: Surface, src: Rect, out: Rect): IBinder = unwrap {
        val display = sc.getMethod("createDisplay", String::class.java, Boolean::class.javaPrimitiveType)
            .invoke(null, name, false) as IBinder
        sc.getMethod("openTransaction").invoke(null)
        try {
            sc.getMethod("setDisplaySurface", IBinder::class.java, Surface::class.java).invoke(null, display, surface)
            sc.getMethod("setDisplayProjection", IBinder::class.java, Int::class.javaPrimitiveType, Rect::class.java, Rect::class.java)
                .invoke(null, display, 0, src, out)
            sc.getMethod("setDisplayLayerStack", IBinder::class.java, Int::class.javaPrimitiveType)
                .invoke(null, display, layerStack())
        } finally {
            sc.getMethod("closeTransaction").invoke(null)
        }
        display
    }

    fun destroy(display: IBinder) = unwrap {
        sc.getMethod("destroyDisplay", IBinder::class.java).invoke(null, display)
        Unit
    }

    /** The reflected call's own exception, not InvocationTargetException. */
    private inline fun <T> unwrap(block: () -> T): T = try {
        block()
    } catch (e: java.lang.reflect.InvocationTargetException) {
        throw e.targetException
    }
}
