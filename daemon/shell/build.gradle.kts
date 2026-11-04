/*
 * The app_process daemon: Main is started by app_process with this APK on
 * CLASSPATH (app_process only needs classes.dex inside a zip) and runs as
 * the shell uid. `./gradlew mobaxJar` copies it to build/mobax.jar.
 */
plugins {
    id("com.android.application")
}

android {
    namespace = "dev.mobileax.daemon"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.mobileax.daemon"
        minSdk = 28
        targetSdk = 36
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles("../core/proguard-rules.pro", "proguard-rules.pro")
            // app_process doesn't check signatures: leave it unsigned.
            signingConfig = null
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
}
