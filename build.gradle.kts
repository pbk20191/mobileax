/*
 * Build for the on-device daemon: src/ is shared, and two flavors start it
 * differently.
 *
 *  - shell (src-shell/): Main is started by app_process with the APK on
 *    CLASSPATH (app_process only needs classes.dex inside a zip) and runs as
 *    the shell uid. mobaxJar copies it to build/mobax.jar, the path the CLI,
 *    installer and release workflow expect.
 *  - instr (src-instr/): installed and run as an instrumentation (`am
 *    instrument … dev.mobileax.daemon/.MobaxInstrumentation`), an app uid
 *    with the shell's permissions adopted. mobaxApk copies it to
 *    build/mobaxd.apk.
 *
 * Not a launchable app either way. AGP handles the SDK (platform +
 * build-tools, downloaded if missing), Kotlin, and R8.
 *
 *   ./gradlew mobaxJar mobaxApk              build both
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
        minSdk = 28
        targetSdk = 36
    }

    flavorDimensions += "start"
    productFlavors {
        create("shell") {
            dimension = "start"
            proguardFiles("proguard-shell.pro")
        }
        create("instr") {
            dimension = "start"
            // Shell commands go through UiAutomation.executeShellCommand, which needs 29 for files (UaShell).
            minSdk = 29
            proguardFiles("proguard-instr.pro")
            // Installing needs a signature, not a trusted one: the debug key
            // (generated per machine/CI runner). A device holding another
            // build's signature gets it uninstalled and reinstalled by `mobax use`.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    sourceSets {
        named("main") {
            manifest.srcFile("AndroidManifest.xml")
            java.setSrcDirs(listOf("src"))
            kotlin.setSrcDirs(listOf("src"))
            res.setSrcDirs(emptyList<String>())
            resources.setSrcDirs(emptyList<String>())
        }
        named("shell") {
            java.setSrcDirs(listOf("src-shell"))
            kotlin.setSrcDirs(listOf("src-shell"))
        }
        named("instr") {
            manifest.srcFile("AndroidManifest-instr.xml")
            java.setSrcDirs(listOf("src-instr"))
            kotlin.setSrcDirs(listOf("src-instr"))
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles("proguard-rules.pro")
            // app_process doesn't check signatures: the shell flavor stays
            // unsigned; instr sets its own.
            signingConfig = null
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
    implementation("androidx.test.uiautomator:uiautomator:2.4.0")
    "instrImplementation"("androidx.test:runner:1.7.0")
    "instrImplementation"("androidx.test:monitor:1.8.0")
}

fun copyApk(name: String, variant: String, out: String, what: String) = tasks.register<Copy>(name) {
    group = "build"
    description = what
    from(tasks.named("assemble${variant.replaceFirstChar { it.uppercase() }}Release").map {
        layout.buildDirectory.dir("outputs/apk/$variant/release")
    }) {
        include("*.apk")
        rename { out }
    }
    into(layout.projectDirectory.dir("build"))
}

val mobaxJar = copyApk("mobaxJar", "shell", "mobax.jar", "Builds build/mobax.jar (the shell flavor) for app_process.")
val mobaxApk = copyApk("mobaxApk", "instr", "mobaxd.apk", "Builds build/mobaxd.apk (the instr flavor) for am instrument.")

tasks.named("assemble") { dependsOn(mobaxJar, mobaxApk) }
