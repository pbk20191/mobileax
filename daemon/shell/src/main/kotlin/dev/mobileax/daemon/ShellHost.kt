package dev.mobileax.daemon

import android.app.Application
import android.app.Instrumentation
import android.app.UiAutomation
import android.content.Context
import android.content.ContextWrapper
import android.content.pm.ApplicationInfo
import android.os.Bundle
import android.os.Looper
import androidx.test.platform.app.InstrumentationRegistry
import android.os.ParcelFileDescriptor
import dev.mobileax.daemon.Shell.Input
import dev.mobileax.daemon.Shell.ShellProc
import kotlinx.coroutines.withContext
import java.io.File
import java.io.IOException
import java.io.OutputStream
import kotlin.concurrent.Volatile
import kotlin.system.exitProcess

/**
 * The app_process build: the daemon runs as the shell uid (2000), with no
 * ActivityThread of its own, so it builds its contexts itself.
 */
internal object ShellContexts {
    /** ActivityThread.systemMain() binds to the calling thread's looper: first call on main. */
    private val systemContextLazy: Context by lazy {
        try {
            if (Looper.getMainLooper() == null) {
                // app_process has no ActivityThread to prepare the main looper for us.
                @Suppress("DEPRECATION")
                Looper.prepareMainLooper()
            }
            val at = Class.forName("android.app.ActivityThread")
            val inst = at.getMethod("systemMain").invoke(null)
            at.getMethod("getSystemContext").invoke(inst) as Context
        } catch (t: Throwable) {
            throw RuntimeException("system Context lookup failed", t)
        }
    }

    fun systemContext(): Context = systemContextLazy

    /**
     * The daemon's Application, as com.android.shell: the package of the uid
     * we run as. systemMain() already made one, but on the "android" package,
     * so its op package and AttributionSource name a package uid 2000 doesn't
     * own and the services that check (virtual displays, app ops) refuse it.
     * Installed as ActivityThread.mInitialApplication so currentOpPackageName()
     * and currentAttributionSource() agree. If building it fails, the system
     * context with the shell's package name forced on it.
     */
    private val appContextLazy: Context by lazy {
        try {
            shellApplication()
        } catch (t: Throwable) {
            Logx.bootWarn("shell Application failed; using the system context", t)
            object : ContextWrapper(systemContextLazy) {
                override fun getPackageName(): String = SHELL_PACKAGE
                override fun getOpPackageName(): String = SHELL_PACKAGE
            }
        }
    }

    fun appContext(): Context = appContextLazy

    private fun shellApplication(): Application {
        val sys = systemContextLazy
        val atCls = Class.forName("android.app.ActivityThread")
        val at = atCls.getMethod("currentActivityThread").invoke(null)
        val info = sys.packageManager.getApplicationInfo(SHELL_PACKAGE, 0)
        val compatCls = Class.forName("android.content.res.CompatibilityInfo")
        val compat = compatCls.getField("DEFAULT_COMPATIBILITY_INFO").get(null)
        val pkg = atCls.getMethod("getPackageInfoNoCheck", ApplicationInfo::class.java, compatCls)
            .invoke(at, info, compat)!!
        // makeApplication points the thread's context class loader at
        // Shell.apk; ours (the daemon jar) has to stay.
        val thread = Thread.currentThread()
        val loader = thread.contextClassLoader
        val app = try {
            pkg.javaClass.getMethod("makeApplication", Boolean::class.javaPrimitiveType, Instrumentation::class.java)
                .invoke(pkg, true, null) as Application
        } finally {
            thread.contextClassLoader = loader
        }
        atCls.getDeclaredField("mInitialApplication").apply { isAccessible = true }.set(at, app)
        return app
    }

    const val SHELL_PACKAGE = "com.android.shell"
}

/** The app_process daemon: the shell uid, com.android.shell's package. */
internal object ShellHost : Host {
    override val context: Context get() = ShellContexts.appContext()
    override val callerPackage: String = ShellContexts.SHELL_PACKAGE
    /** Not installed as a package of its own: nothing to hide from package lists. */
    override val selfPackage: String? = null
    /** The host redirects stdout and stderr to /data/local/tmp/mobax.log. */
    override fun startupLine(line: String) = println(line)
    @Volatile private var standIn: Instrumentation? = null

    /**
     * No instrumentation runs here: a stand-in handing out our UiAutomation
     * and Context, registered with InstrumentationRegistry so androidx.test
     * code that looks it up (UiDevice.getInstance(), Configurator users)
     * finds it, as it would in the instrumentation daemon. Main calls this
     * once UiAutomation is connected.
     */
    fun registerInstrumentation(ua: UiAutomation) {
        val i = StandIn(ua)
        standIn = i
        InstrumentationRegistry.registerInstance(i, Bundle())
    }

    override fun instrumentation(ua: UiAutomation): Instrumentation =
        standIn ?: StandIn(ua).also { standIn = it }

    private class StandIn(private val ua: UiAutomation) : Instrumentation() {
        override fun getUiAutomation(): UiAutomation = ua
        override fun getUiAutomation(flags: Int): UiAutomation = ua
        override fun getContext(): Context = ShellContexts.appContext()
        override fun getTargetContext(): Context = ShellContexts.appContext()
    }

    /** We are the shell: the command is our own child, `sh -c`. */
    override fun openShell(cmd: String, mergeErr: Boolean, input: Input): ShellProc = ChildProc(cmd, mergeErr, input)

    override fun exit(reason: String) {
        Logx.boot("mobaxd $reason; exiting")
        exitProcess(0)
    }
}

private class ChildProc(cmd: String, mergeErr: Boolean, input: Input) : ShellProc() {
    private val p: Process
    private val pid: Int
    override val stdout: ParcelFileDescriptor
    override val stdin: OutputStream?
    private val errReader: Thread?
    @Volatile private var errText = ""
    @Volatile private var ended = false

    init {
        // stdout goes into a pipe we own (via /proc/self/fd/N): the stream
        // ProcessBuilder hands out is drained and closed by its reaper thread
        // on exit, so its fd can't be watched from the looper.
        val (r, w) = ParcelFileDescriptor.createPipe()
        val pb = ProcessBuilder("/system/bin/sh", "-c", cmd)
            .redirectOutput(File("/proc/self/fd/${w.fd}"))
            .redirectErrorStream(mergeErr)
        if (input == Input.NONE) pb.redirectInput(File("/dev/null"))
        p = try { pb.start() } catch (e: IOException) { r.close(); throw e } finally { w.close() }
        stdout = r
        stdin = if (input == Input.DATA) p.outputStream else null
        pid = try {
            p.javaClass.getDeclaredField("pid").apply { isAccessible = true }.getInt(p)
        } catch (_: Throwable) { 0 }
        errReader = if (mergeErr) null else Thread({
            errText = try { p.errorStream.readBytes().toString(Charsets.UTF_8) } catch (_: IOException) { "" }
        }, "mobax-sh-err").apply { isDaemon = true; start() }
    }

    override suspend fun exitCode(): Int = withContext(Loop.Blocking) {
        val code = try { p.waitFor() } catch (_: InterruptedException) { -1 }
        ended = true
        errReader?.join(1_000)
        code
    }

    override val errOutput: String get() = errText

    override fun kill() {
        try { p.outputStream.close() } catch (_: IOException) {}
        try { stdout.close() } catch (_: IOException) {}
        if (!ended) {
            Shell.terminate(pid)   // its children too
            p.destroy()
        }
    }
}
