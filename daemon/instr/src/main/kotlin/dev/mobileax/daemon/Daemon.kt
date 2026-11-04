package dev.mobileax.daemon

import android.app.Instrumentation
import android.app.UiAutomation
import android.content.Context
import android.os.Bundle
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.Configurator
import androidx.test.uiautomator.UiDevice

/**
 * Process-wide handles, all read from [InstrumentationRegistry] (registered
 * by [MobaxInstrumentation.onCreate]); nothing here holds state of its own.
 */

/** One set of flags for every UiAutomation user: a different set makes
 *  `getUiAutomation(flags)` reconnect, which would drop the other's state. */
internal const val UA_FLAGS: Int = UiAutomation.FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES

internal val instr: Instrumentation get() = InstrumentationRegistry.getInstrumentation()

/** The instrumentation: our own package (dev.mobileax.daemon), shell permissions adopted on top. */
internal object InstrHost : Host {
    override val context: Context get() = instr.context
    override val callerPackage: String get() = context.opPackageName
    /** Installed as a package of its own: hidden from package lists. */
    override val selfPackage: String? get() = context.packageName

    /** A line for mobax.log through `am instrument`; stderr (logcat) before
     *  the instrumentation is registered. */
    override fun startupLine(line: String) {
        val i = try { InstrumentationRegistry.getInstrumentation() } catch (_: IllegalStateException) { null }
        if (i == null) System.err.println(line)
        else i.sendStatus(0, Bundle().apply { putString("mobaxd", line) })
    }

    override fun openShell(cmd: String, mergeErr: Boolean, input: Shell.Input): Shell.ShellProc =
        UaShell.open(cmd, mergeErr, input)
}

/** Connected on first use; the same flags return the cached instance. */
internal val ua: UiAutomation get() = instr.getUiAutomation(UA_FLAGS)

internal val device: UiDevice by lazy {
    Configurator.getInstance().uiAutomationFlags = UA_FLAGS
    UiDevice.getInstance(instr)
}
