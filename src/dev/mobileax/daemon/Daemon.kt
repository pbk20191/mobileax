package dev.mobileax.daemon

import android.app.Instrumentation
import android.app.UiAutomation
import android.content.Context
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

/** Our own package (dev.mobileax.daemon); shell permissions are adopted on top. */
internal val ctx: Context get() = instr.context

/** Connected on first use; the same flags return the cached instance. */
internal val ua: UiAutomation get() = instr.getUiAutomation(UA_FLAGS)

internal val device: UiDevice by lazy {
    Configurator.getInstance().uiAutomationFlags = UA_FLAGS
    UiDevice.getInstance(instr)
}
