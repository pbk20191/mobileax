/*
 * The instrumentation daemon: installed and run as an instrumentation
 * (`am instrument … dev.mobileax.daemon/.MobaxInstrumentation`), an app uid
 * with the shell's permissions adopted. It instruments itself, so one APK
 * is the whole install. `./gradlew mobaxApk` copies it to build/mobaxd.apk.
 */
plugins {
    id("com.android.application")
}

android {
    namespace = "dev.mobileax.daemon"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.mobileax.daemon"
        minSdk = 29
        targetSdk = 36
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles("../core/proguard-rules.pro", "proguard-rules.pro")
            // Installing needs a signature, not a trusted one: the debug key
            // (generated per machine/CI runner). A device holding another
            // build's signature gets it uninstalled and reinstalled by `mobax use`.
            signingConfig = signingConfigs.getByName("debug")
            vcsInfo.include = false
        }
    }

    packaging {
        // Compress classes.dex: the file is pushed over adb and shipped in
        // release tarballs, and nothing gains from mapping it uncompressed.
        dex.useLegacyPackaging = true
        resources {
            // Kotlin reflection/debug metadata the daemon never reads. Keep
            // META-INF/services: Dispatchers.Main is found through it.
            excludes += listOf("kotlin/**", "DebugProbesKt.bin", "META-INF/*.version")
        }
    }

    dependenciesInfo {
        includeInApk = false
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
    implementation(project(":daemon:core"))
    implementation("androidx.test:runner:1.7.0")
    implementation("androidx.test:monitor:1.8.0")
}
