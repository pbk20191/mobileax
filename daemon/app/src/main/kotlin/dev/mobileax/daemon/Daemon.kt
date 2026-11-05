package dev.mobileax.daemon

import android.app.Activity
import android.app.Instrumentation
import android.app.UiAutomation
import android.content.Context
import android.os.Bundle
import androidx.test.platform.app.InstrumentationRegistry
import kotlin.system.exitProcess

/**
 * Process-wide handles, all read from [InstrumentationRegistry] (registered
 * by [MobaxInstrumentation.onCreate]); nothing here holds state of its own.
 */

/** One set of flags for every UiAutomation user: a different set makes
 *  `getUiAutomation(flags)` reconnect, which would drop the other's state. */
internal const val UA_FLAGS: Int = UiAutomation.FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES

internal val instr: Instrumentation get() = InstrumentationRegistry.getInstrumentation()

/**
 * The instrumentation: this androidTest APK (dev.mobileax.daemon.instr)
 * running in the process of the package it instruments (dev.mobileax.daemon),
 * whose uid we are, shell permissions adopted on top. So the Context is the
 * target's: its package matches our uid for binder calls and its files.
 */
internal object InstrHost : Host {
    override val context: Context get() = instr.targetContext
    override val callerPackage: String get() = context.opPackageName
    /** Installed as packages of their own: hidden from package lists. */
    override val selfPackage: String? get() = context.packageName

    /** A line for mobax.log through `am instrument`; stderr (logcat) before
     *  the instrumentation is registered. */
    override fun startupLine(line: String) {
        val i = try { InstrumentationRegistry.getInstrumentation() } catch (_: IllegalStateException) { null }
        if (i == null) System.err.println(line)
        else i.sendStatus(0, Bundle().apply { putString("mobaxd", line) })
    }

    /** Ours: UiDevice gets the UiAutomation the framework connected for it. */
    override fun instrumentation(ua: UiAutomation): Instrumentation = instr

    override fun openShell(cmd: String, mergeErr: Boolean, input: Shell.Input): Shell.ShellProc =
        UaShell.open(cmd, mergeErr, input)

    /**
     * finish(), as AndroidJUnitRunner ends a run: AMS reports the result to
     * `am instrument` and kills the process. Off the main thread, where
     * MonitoringInstrumentation waits for activities (it throws on main).
     * If AMS hasn't killed us within [FINISH_GRACE_MS], exit anyway.
     */
    override fun exit(reason: String) {
        Logx.boot("mobaxd $reason; finishing")
        Thread({
            try {
                instr.finish(Activity.RESULT_OK, Bundle().apply { putString("mobaxd", reason) })
            } catch (t: Throwable) {
                Logx.w("finish failed", t)
            }
            Thread.sleep(FINISH_GRACE_MS)
            exitProcess(0)
        }, "mobax-finish").start()
    }

    private const val FINISH_GRACE_MS = 5_000L
}

/** Connected on first use; the same flags return the cached instance. */
internal val ua: UiAutomation get() = instr.getUiAutomation(UA_FLAGS)

