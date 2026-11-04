/*
 * Build for the on-device daemon, three modules under daemon/:
 *
 *  - core:  the daemon itself (a library)
 *  - shell: started by app_process as the shell uid → build/mobax.jar
 *  - instr: installed and run by `am instrument` → build/mobaxd.apk
 *
 * Not a launchable app either way. AGP handles the SDK (platform +
 * build-tools, downloaded if missing), Kotlin, and R8.
 *
 *   ./gradlew mobaxJar mobaxApk              build both
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

val mobaxJar = copyApk("mobaxJar", ":daemon:shell", "mobax.jar", "Builds build/mobax.jar for app_process.")
val mobaxApk = copyApk("mobaxApk", ":daemon:instr", "mobaxd.apk", "Builds build/mobaxd.apk for am instrument.")

tasks.register("assemble") {
    group = "build"
    dependsOn(mobaxJar, mobaxApk)
}
