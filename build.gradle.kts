/*
 * Build for the on-device daemon, two modules under daemon/:
 *
 *  - core: the daemon itself (a library)
 *  - app:  its APK → build/mobaxd.apk, run by app_process as the shell
 *          uid; and the androidTest APK that instruments it →
 *          build/mobaxd-instr.apk, for `am instrument`
 *
 * Not a launchable app either way. AGP handles the SDK (platform +
 * build-tools, downloaded if missing), Kotlin, and R8.
 *
 *   ./gradlew mobaxApk                       build both APKs
 *   ./gradlew benchToastApk                  the agent benchmark's toast app
 */

// Gradle's own outputs stay out of build/, which holds only the deployables.
layout.buildDirectory = layout.projectDirectory.dir("build-gradle")

fun copyApk(name: String, module: String, out: String, what: String) = tasks.register<Copy>(name) {
    group = "build"
    description = what
    dependsOn("$module:assembleRelease")
    from(project(module).layout.buildDirectory.dir("outputs/apk/release")) {
        include("*.apk")
        rename { out }
    }
    into(layout.projectDirectory.dir("build"))
}

val mobaxApk = tasks.register<Copy>("mobaxApk") {
    group = "build"
    description = "Builds build/mobaxd.apk (app_process, and the instrumented package) and build/mobaxd-instr.apk (am instrument)."
    val app = project(":daemon:app")
    dependsOn(":daemon:app:assembleRelease", ":daemon:app:assembleReleaseAndroidTest")
    from(app.layout.buildDirectory.dir("outputs/apk/release")) {
        include("*.apk")
        rename { "mobaxd.apk" }
    }
    from(app.layout.buildDirectory.dir("outputs/apk/androidTest/release")) {
        include("*.apk")
        rename { "mobaxd-instr.apk" }
    }
    into(layout.projectDirectory.dir("build"))
}
copyApk("benchToastApk", ":bench:toast-app", "bench-toast.apk", "Builds build/bench-toast.apk for the agent benchmark (s6).")

tasks.register("assemble") {
    group = "build"
    dependsOn(mobaxApk)
}
