/*
 * Build for the on-device daemon in src/.
 *
 * Not a launchable app: the APK is installed and run as an instrumentation
 * (`am instrument -w … dev.mobileax.daemon/.MobaxInstrumentation`). AGP
 * handles the SDK (platform + build-tools, downloaded if missing), Kotlin,
 * and R8; mobaxJar copies the release APK to build/mobax.jar, the path the
 * CLI, installer and release workflow expect (the name predates the APK).
 *
 *   ./gradlew mobaxJar                       build build/mobax.jar
 */
plugins {
    id("com.android.application")
}

// Gradle's own outputs stay out of build/, which holds only the deployable jar.
layout.buildDirectory = layout.projectDirectory.dir("build-gradle")

android {
    namespace = "dev.mobileax.daemon"
    compileSdk = 36

    defaultConfig {
        minSdk = 29
        targetSdk = 36
    }

    sourceSets {
        named("main") {
            manifest.srcFile("AndroidManifest.xml")
            java.setSrcDirs(listOf("src"))
            kotlin.setSrcDirs(listOf("src"))
            res.setSrcDirs(emptyList<String>())
            resources.setSrcDirs(emptyList<String>())
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles("proguard-rules.pro")
            // Installing needs a signature, not a trusted one: the debug key
            // (generated per machine/CI runner). A device holding another
            // build's signature gets it uninstalled and reinstalled by `mobax use`.
            signingConfig = signingConfigs.getByName("debug")
            vcsInfo.include = false
        }
    }

    packaging {
        // Compress classes.dex: mobax.jar is pushed over adb and shipped in release
        // tarballs, and nothing gains from mapping it uncompressed.
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
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
    implementation("androidx.test:runner:1.7.0")
    implementation("androidx.test:monitor:1.8.0")
    implementation("androidx.test.uiautomator:uiautomator:2.4.0")
}

val mobaxJar = tasks.register<Copy>("mobaxJar") {
    group = "build"
    description = "Builds build/mobax.jar (the release APK, installed and run via am instrument)."
    from(tasks.named("assembleRelease").map { layout.buildDirectory.dir("outputs/apk/release") }) {
        include("*.apk")
        rename { "mobax.jar" }
    }
    into(layout.projectDirectory.dir("build"))
}

tasks.named("assemble") { dependsOn(mobaxJar) }
