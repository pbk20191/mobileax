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

    // One key for every build, committed with its password, as Appium does
    // for its UiAutomator2 server: the signature proves nothing (installing
    // takes adb anyway), it only has to stay the same, so a device keeps
    // its copy across this machine's, another's and CI's builds instead of
    // `mobax use` uninstalling it for a different debug key.
    signingConfigs {
        create("mobaxd") {
            storeFile = file("mobaxd.p12")
            storeType = "PKCS12"
            storePassword = "mobileax"
            keyAlias = "mobaxd"
            keyPassword = "mobileax"
        }
    }

    buildTypes {
        debug {
            signingConfig = signingConfigs.getByName("mobaxd")
        }
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles("../core/proguard-rules.pro", "proguard-rules.pro")
            signingConfig = signingConfigs.getByName("mobaxd")
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
