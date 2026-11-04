package dev.mobileax.daemon

import java.lang.reflect.Method
import java.nio.charset.StandardCharsets

/**
 * Direct `SystemProperties` access. Reaches the same in-process key/value
 * store that `/system/bin/getprop` mmaps — but without the per-call
 * process spawn that `adb shell getprop` pays.
 * 
 * Reads are essentially free. Writes go to `setprop` as the shell (through
 * [Shell]): init grants property prefixes by SELinux domain, and the shell's
 * are the ones that matter; ours, an app's, are next to none.
 */
internal class Props {
    private val get: Method

    init {
        val sp = Class.forName("android.os.SystemProperties")
        this.get = sp.getDeclaredMethod("get", String::class.java)
    }

    fun doGet(key: String?): ByteArray {
        try {
            val v = get.invoke(null, key)
            return (v as String).toByteArray(StandardCharsets.UTF_8)
        } catch (t: Throwable) {
            return err("get prop-failed:" + t.message)
        }
    }

    fun doSet(key: String?, value: String?): ByteArray {
        try {
            val r = Shell.runBlocking(Shell.argv(listOf("setprop", key ?: "", value ?: "")) + " 2>&1")
            if (r.exit != 0) return err("set prop-failed:" + r.out.trim().ifEmpty { "exit ${r.exit}" })
            return "ok".toByteArray(StandardCharsets.UTF_8)
        } catch (t: Throwable) {
            return err("set prop-failed:" + t.message)
        }
    }

    companion object {
        private fun err(tail: String?): ByteArray {
            return ("ERR:$tail").toByteArray(StandardCharsets.UTF_8)
        }
    }
}
