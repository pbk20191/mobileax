package dev.mobileax.daemon

import android.os.Bundle
import android.util.Log
import androidx.test.platform.app.InstrumentationRegistry

/**
 * Daemon logging. Runtime diagnostics go to logcat under tag `mobaxd`
 * (`mobax logcat -s mobaxd:*`). Startup lines also go out as instrumentation
 * status, which `am instrument -w` prints (`INSTRUMENTATION_STATUS: mobaxd=…`)
 * into /data/local/tmp/mobax.log; the host shows that file when the daemon
 * fails to come up.
 */
internal object Logx {
    private const val TAG = "mobaxd"
    const val STATUS_KEY = "mobaxd"

    /** A line for mobax.log through `am instrument`; stderr (logcat) before
     *  the instrumentation is registered. */
    fun status(line: String) {
        val i = try { InstrumentationRegistry.getInstrumentation() } catch (_: IllegalStateException) { null }
        if (i == null) System.err.println(line)
        else i.sendStatus(0, Bundle().apply { putString(STATUS_KEY, line) })
    }

    fun i(msg: String) {
        Log.i(TAG, msg)
    }

    fun w(msg: String, t: Throwable? = null) {
        Log.w(TAG, msg, t)
    }

    fun e(msg: String, t: Throwable? = null) {
        Log.e(TAG, msg, t)
    }

    /** Startup lifecycle line: mobax.log and logcat. */
    fun boot(msg: String) {
        status(msg)
        Log.i(TAG, msg)
    }

    /** Startup problem worth seeing in mobax.log when the daemon fails to start. */
    fun bootWarn(msg: String, t: Throwable? = null) {
        status(if (t == null) "warn: $msg" else "warn: $msg: $t")
        Log.w(TAG, msg, t)
    }

    /** Fatal startup error: full stack to mobax.log and logcat. */
    fun fatal(msg: String, t: Throwable) {
        status("fatal: $msg: $t")
        Log.e(TAG, msg, t)
    }
}
