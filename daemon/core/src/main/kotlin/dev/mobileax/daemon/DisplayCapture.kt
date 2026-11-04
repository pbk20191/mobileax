package dev.mobileax.daemon

import android.graphics.Bitmap
import android.graphics.Rect
import android.os.Build
import android.os.IBinder
import kotlin.concurrent.Volatile

/**
 * One frame of the default display, composited by SurfaceFlinger at the
 * size asked for: [scale] and [crop] are applied while compositing, so a
 * preview or one element costs a fraction of a full-resolution capture
 * (measured on SDK 36: 383×852 in ~17 ms against ~58 ms for
 * UiAutomation.takeScreenshot, which always makes, copies and sends the
 * full screen first).
 *
 * The screenshot fallback when no mirror can be made (the mirror needs
 * CAPTURE_VIDEO_OUTPUT; capturing needs READ_FRAME_BUFFER, which the shell
 * holds and the instrumentation adopts). Hidden APIs, one per SDK range:
 *
 *  - 34+:   IWindowManager.captureDisplay (WMS checks the permission in Java)
 *  - 31–33: SurfaceControl.captureDisplay(DisplayCaptureArgs)
 *  - 29–30: SurfaceControl.screenshot(Rect, w, h, rotation)
 *
 * The first failure turns it off for the process; callers then use
 * UiAutomation.takeScreenshot.
 */
internal object DisplayCapture {
    @Volatile private var off = false

    /** A software bitmap of [crop] (screen coordinates; null = whole screen)
     *  scaled by [scale], or null when this path is unavailable. [rotation]
     *  is the display's (Surface.ROTATION_*). */
    fun capture(scale: Float, crop: Rect?, screenW: Int, screenH: Int, rotation: Int): Bitmap? {
        if (off) return null
        // 29–30 capture in the natural orientation: a turned screen comes out squeezed.
        if (Build.VERSION.SDK_INT < 31 && rotation != 0) return null
        return try {
            val hw = when {
                Build.VERSION.SDK_INT >= 34 -> viaWindowManager(scale, crop)
                Build.VERSION.SDK_INT >= 31 -> viaCaptureArgs(scale, crop, screenW, screenH)
                else -> viaScreenshot(scale, crop, screenW, screenH)
            } ?: error("no buffer")
            if (hw.config != Bitmap.Config.HARDWARE) hw
            else hw.copy(Bitmap.Config.ARGB_8888, false).also { hw.recycle() } ?: error("copy failed")
        } catch (t: Throwable) {
            off = true
            val c = generateSequence(t) { it.cause }.last()
            Logx.w("screenshot: display capture unavailable (${c.javaClass.simpleName}: ${c.message}); using UiAutomation")
            null
        }
    }

    private val windowManager: Any by lazy {
        Binders.asInterface("android.view.IWindowManager\$Stub", Binders.service("window"))!!
    }

    private fun viaWindowManager(scale: Float, crop: Rect?): Bitmap? {
        val sc = Class.forName("android.window.ScreenCapture")
        val args = captureArgs("android.window.ScreenCapture\$CaptureArgs\$Builder", null, scale, crop)
        val listener = sc.getMethod("createSyncCaptureListener").invoke(null)
        val wm = windowManager
        wm.javaClass.methods.first { it.name == "captureDisplay" && it.parameterTypes.size == 3 }
            .invoke(wm, 0, args, listener)
        val shb = listener.javaClass.getMethod("getBuffer").invoke(listener) ?: return null
        return shb.javaClass.getMethod("asBitmap").invoke(shb) as Bitmap?
    }

    private fun viaCaptureArgs(scale: Float, crop: Rect?, screenW: Int, screenH: Int): Bitmap? {
        val scCls = Class.forName("android.view.SurfaceControl")
        val token = scCls.getMethod("getInternalDisplayToken").invoke(null) as IBinder
        val args = captureArgs("android.view.SurfaceControl\$DisplayCaptureArgs\$Builder", token, scale, crop ?: Rect(0, 0, screenW, screenH)) { b, src ->
            // A display capture here sizes by setSize; frameScale is for layer captures.
            b.javaClass.getMethod("setSize", Int::class.javaPrimitiveType, Int::class.javaPrimitiveType)
                .invoke(b, maxOf(1, (src.width() * scale).toInt()), maxOf(1, (src.height() * scale).toInt()))
        }
        val shb = scCls.getMethod("captureDisplay", args.javaClass).invoke(null, args) ?: return null
        return shb.javaClass.getMethod("asBitmap").invoke(shb) as Bitmap?
    }

    /** Builds CaptureArgs through its builder; [token] for the display builder's constructor,
     *  [more] for builder calls of one SDK range. */
    private fun captureArgs(
        builder: String, token: IBinder?, scale: Float, crop: Rect?, more: (Any, Rect) -> Unit = { _, _ -> },
    ): Any {
        val bCls = Class.forName(builder)
        val b = if (token == null) bCls.getDeclaredConstructor().newInstance()
        else bCls.getDeclaredConstructor(IBinder::class.java).newInstance(token)
        bCls.getMethod("setFrameScale", Float::class.javaPrimitiveType).invoke(b, scale)
        if (crop != null) {
            bCls.getMethod("setSourceCrop", Rect::class.java).invoke(b, crop)
            more(b, crop)
        }
        return bCls.getMethod("build").invoke(b)!!
    }

    private fun viaScreenshot(scale: Float, crop: Rect?, screenW: Int, screenH: Int): Bitmap? {
        val src = crop ?: Rect(0, 0, screenW, screenH)
        val w = maxOf(1, (src.width() * scale).toInt())
        val h = maxOf(1, (src.height() * scale).toInt())
        val m = Class.forName("android.view.SurfaceControl").getMethod(
            "screenshot", Rect::class.java, Int::class.javaPrimitiveType, Int::class.javaPrimitiveType, Int::class.javaPrimitiveType,
        )
        // Rotation 0: the crop and size are in the current orientation.
        return m.invoke(null, crop ?: Rect(), w, h, 0) as Bitmap?
    }
}
