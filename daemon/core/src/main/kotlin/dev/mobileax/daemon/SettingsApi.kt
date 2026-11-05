package dev.mobileax.daemon

import android.content.ContentResolver
import android.content.Context
import android.provider.Settings
import java.nio.charset.StandardCharsets
import java.util.Locale

/**
 * settings get/put — direct ContentResolver against the Settings provider.
 * Skips both the `adb shell` hop and the `/system/bin/settings`
 * process spawn.
 * 
 * Shell UID has WRITE_SECURE_SETTINGS (granted by adbd), so all three
 * namespaces (system / secure / global) are writable from here, same as
 * `adb shell settings put …`.
 */
internal class SettingsApi(ctx: Context) {
    private val cr: ContentResolver? = ctx.contentResolver
    private val direct = SettingsDirect()

    fun get(namespace: String, key: String?): ByteArray {
        // Fast path: direct IContentProvider.call via getContentProviderExternal.
        var v = direct.tryGet(namespace, key)
        if (v != null) return v.toByteArray(StandardCharsets.UTF_8)
        // Second try: public ContentResolver helpers. Will usually fail from
        // app_process (no IApplicationThread registration) but is harmless to try.
        try {
            v = when (namespace.lowercase(Locale.getDefault())) {
                "system" -> Settings.System.getString(cr, key)
                "secure" -> Settings.Secure.getString(cr, key)
                "global" -> Settings.Global.getString(cr, key)
                else -> return err("bad-namespace:$namespace")
            }
            if (v != null) return v.toByteArray(StandardCharsets.UTF_8)
        } catch (_: Throwable) {
        }
        // Last resort: spawn the settings binary.
        return execFallback("get", namespace, key, null)
    }

    fun put(namespace: String, key: String?, value: String?): ByteArray {
        if (direct.tryPut(namespace, key, value)) {
            return "ok".toByteArray(StandardCharsets.UTF_8)
        }
        try {
            val ok: Boolean = when (namespace.lowercase(Locale.getDefault())) {
                "system" -> Settings.System.putString(cr, key, value)
                "secure" -> Settings.Secure.putString(cr, key, value)
                "global" -> Settings.Global.putString(cr, key, value)
                else -> return err("bad-namespace:$namespace")
            }
            if (ok) return "ok".toByteArray(StandardCharsets.UTF_8)
        } catch (_: Throwable) {
        }
        return execFallback("put", namespace, key, value)
    }

    private fun execFallback(op: String?, ns: String?, key: String?, value: String?): ByteArray {
        try {
            val args = if ("get" == op) listOf("settings", "get", ns ?: "", key ?: "")
            else listOf("settings", "put", ns ?: "", key ?: "", value ?: "")
            val r = Shell.runBlocking(Shell.argv(args) + " 2>&1")
            val code = r.exit
            val out = r.out.trim { it <= ' ' }
            if (code != 0) return err("settings-exit-" + code + (if (out.isEmpty()) "" else ":$out"))
            return (if ("get" == op)
                (out.ifEmpty { "null" })
            else
                "ok").toByteArray(StandardCharsets.UTF_8)
        } catch (e: Exception) {
            return err("settings-fallback-failed:" + e.message)
        }
    }

    companion object {
        private fun err(tail: String?): ByteArray {
            return ("ERR:$tail").toByteArray(StandardCharsets.UTF_8)
        }
    }
}
