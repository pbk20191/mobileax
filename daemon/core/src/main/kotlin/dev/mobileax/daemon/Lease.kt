package dev.mobileax.daemon

import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * Ends the daemon once no client has been connected for [lingerMs].
 *
 * Every open connection holds the daemon: the host's state watcher, the MCP
 * server's pooled sessions, a mirror. When adb goes away (cable out, host
 * gone, `adb kill-server`) adbd closes them all, and after the grace period
 * the daemon leaves instead of holding UiAutomation on a device nobody
 * drives. The next `use` starts it again.
 *
 * On a physical device that is the default. An emulator belongs to the host
 * that runs it, so there the daemon stays ([lingerMs] 0) unless told
 * otherwise with `idle_exit`. Main thread only.
 */
internal object Lease {
    private const val DEVICE_LINGER_MS = 30_000L

    /** 0: never leave on its own. */
    private var lingerMs = if (isEmulator()) 0L else DEVICE_LINGER_MS
    private var open = 0
    private var timer: Job? = null
    private var onExpire: (reason: String) -> Unit = {}

    /** Arms the timer for the time before the first client. */
    fun start(onExpire: (reason: String) -> Unit) {
        this.onExpire = onExpire
        Logx.boot("mobaxd idle exit ${if (lingerMs > 0) "after ${lingerMs / 1000}s without clients" else "off"}")
        arm()
    }

    fun opened() {
        open++
        disarm()
    }

    fun closed() {
        open--
        if (open == 0) arm()
    }

    /** `idle_exit <seconds>`: 0 keeps the daemon up until `quit`. */
    fun setLinger(ms: Long) {
        lingerMs = ms.coerceAtLeast(0)
        disarm()
        if (open == 0) arm()
    }

    private fun arm() {
        if (lingerMs <= 0 || timer != null) return
        val ms = lingerMs
        timer = Loop.scope.launch {
            delay(ms)
            onExpire("no clients for ${ms / 1000}s")
        }
    }

    private fun disarm() {
        timer?.cancel()
        timer = null
    }

    private fun isEmulator(): Boolean = try {
        val get = Class.forName("android.os.SystemProperties").getMethod("get", String::class.java)
        get.invoke(null, "ro.kernel.qemu") == "1" || get.invoke(null, "ro.boot.qemu") == "1"
    } catch (t: Throwable) {
        Logx.bootWarn("cannot tell an emulator from a device; treating it as a device", t)
        false
    }
}
