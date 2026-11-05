package dev.mobileax.daemon

import android.os.Bundle
import androidx.test.runner.MonitoringInstrumentation

/**
 * Entry point: `am instrument -w --no-hidden-api-checks -e port N
 * dev.mobileax.daemon.instr/dev.mobileax.daemon.MobaxInstrumentation`.
 *
 * The only class of the androidTest APK. It runs in the process of the
 * package it instruments (dev.mobileax.daemon, the daemon APK) and hands
 * over to [startInstrumentation] there: R8 shrinks the daemon APK without
 * seeing this APK, so it calls nothing but what proguard-rules.pro keeps.
 * start() is never called: there are no tests.
 */
class MobaxInstrumentation : MonitoringInstrumentation() {
    override fun onCreate(arguments: Bundle) {
        super.onCreate(arguments)
        startInstrumentation(arguments)
    }
}
