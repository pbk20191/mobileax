package dev.mobileax.daemon

import android.util.Log

/**
 * Daemon logging. Runtime diagnostics go to logcat under tag `mobaxd`
 * (`mobax logcat -s mobaxd:*`). Startup lines also go to
 * /data/local/tmp/mobax.log ([startupLine]: the app_process daemon's stdout,
 * or the instrumentation's status, which `am instrument -w` prints); the
 * host shows that file when the daemon fails to come up.
 */
internal object Logx {
    private const val TAG = "mobaxd"

    private fun status(line: String) = startupLine(line)

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
