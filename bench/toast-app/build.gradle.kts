/*
 * A one-button app for the agent benchmark's toast scenario (s6): tapping
 * "Show toast" shows a toast and nothing else. Settings' Build number used
 * to do this, but it ignores taps while the instrumentation daemon runs the
 * device as a monkey. `./gradlew benchToastApk` → build/bench-toast.apk.
 */
plugins {
    id("com.android.application")
}

android {
    namespace = "dev.mobileax.bench.toast"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.mobileax.bench.toast"
        minSdk = 28
        targetSdk = 36
    }

    buildTypes {
        release {
            // R8 drops the Kotlin runtime AGP adds: a few KB to install on a full emulator.
            isMinifyEnabled = true
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
