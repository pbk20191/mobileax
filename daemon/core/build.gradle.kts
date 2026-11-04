/*
 * The daemon itself, shared by the two ways to start it: daemon/shell
 * (app_process, the shell uid) and daemon/instr (am instrument). Each sets
 * Host.current and calls serve().
 */
plugins {
    id("com.android.library")
}

android {
    namespace = "dev.mobileax.daemon.core"
    compileSdk = 36

    defaultConfig {
        minSdk = 28
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    lint {
        checkReleaseBuilds = false
    }
}

kotlin {
    compilerOptions {
        // Drop Intrinsics.checkNotNull* calls: R8 can't remove them, and the
        // daemon has no Java callers that could pass unexpected nulls.
        freeCompilerArgs.addAll("-Xno-param-assertions", "-Xno-call-assertions", "-Xno-receiver-assertions")
    }
}

dependencies {
    api("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
    api("androidx.test.uiautomator:uiautomator:2.4.0")
}
