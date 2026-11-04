package dev.mobileax.daemon

import android.app.UiAutomation
import android.content.Context
import android.os.HandlerThread
import android.os.Looper
import java.lang.reflect.InvocationTargetException
import kotlin.system.exitProcess

/**
 * Entry point for `app_process … dev.mobileax.daemon.Main --port=N`.
 *
 * The main thread prepares the main looper, starts listening, and then runs
 * `Looper.loop()` forever. UiAutomation is bound to the main looper and
 * connected from a Work thread once the loop is running, because connect()
 * blocks until its service-connected callback is delivered on that looper.
 */
data object Main {

    @JvmStatic
    fun main(vararg args: String) {
        var port = 9008
        for (a in args) if (a.startsWith("--port=")) port = a.substring("--port=".length).toInt()

        Host.current = ShellHost
        try {
            disableHiddenApiRestrictions()
            initLikeActivityThreadMain()
            // app_process has no ActivityThread to prepare the main looper for us.
            @Suppress("DEPRECATION")
            if (Looper.getMainLooper() == null) Looper.prepareMainLooper()
            logSlowMainLooper()
            Loop.init()
            // ActivityThread.systemMain() binds handlers to the calling thread's
            // looper, so the system context must be created here, on main.
            ShellContexts.systemContext()
            // systemMain() names us "system_process" to the debugger.
            setDdmAppName("mobaxd")
            // Everything below runs as this: com.android.shell, our uid's package.
            val ctx = ShellContexts.appContext()
            Logx.boot("mobaxd context=${ctx.opPackageName}")
            exitWithSystemServer()

            serve(port, connect = { ctx to connectUa() }, onInitFailed = { exitProcess(2) })
            Looper.loop()
        } catch (t: Throwable) {
            Logx.fatal("startup failed", t)
            exitProcess(2)
        }
    }

    /**
     * When system_server dies (a soft restart), every binder this process
     * holds is dead for good and each call fails with DeadSystemException.
     * Exit instead: the next client call finds no daemon and starts a fresh
     * one against the new system_server.
     */
    private fun exitWithSystemServer() {
        try {
            Binders.service("activity").linkToDeath({
                Logx.bootWarn("system_server died; exiting so the next call starts a fresh daemon")
                exitProcess(3)
            }, 0)
        } catch (t: Throwable) {
            Logx.bootWarn("cannot watch system_server; a soft restart will leave this daemon stale", t)
        }
    }

    /** Runs on a Work thread: connect() blocks until its callback arrives on the main looper. */
    private fun connectUa(): UiAutomation {
        val ua = try {
            connectedUiAutomation(Looper.getMainLooper()).also { Logx.boot("mobaxd ua-looper=main") }
        } catch (t: Throwable) {
            Logx.bootWarn("UiAutomation on main looper failed; using mobax-ua thread", t)
            val ht = HandlerThread("mobax-ua").apply { start() }
            connectedUiAutomation(ht.looper).also { Logx.boot("mobaxd ua-looper=thread") }
        }
        registerShutdownHook(ua)
        try { ua.setRunAsMonkey(true) } catch (t: Throwable) { Logx.bootWarn("setRunAsMonkey failed", t) }
        return ua
    }

    private fun connectedUiAutomation(looper: Looper): UiAutomation {
        val connCls = Class.forName("android.app.UiAutomationConnection")
        val conn = connCls.getDeclaredConstructor().apply { isAccessible = true }.newInstance()
        val iConnCls = Class.forName("android.app.IUiAutomationConnection")
        val ua = createUiAutomation(looper, conn, iConnCls)
        connectWithRetry(ua)
        return ua
    }

    private fun createUiAutomation(looper: Looper, conn: Any, iConnCls: Class<*>): UiAutomation {
        val ctor = try {
            UiAutomation::class.java.getDeclaredConstructor(Looper::class.java, iConnCls)
        } catch (e: NoSuchMethodException) {
            // Newer SDKs may only expose (Context, IUiAutomationConnection).
            try {
                val c = UiAutomation::class.java.getDeclaredConstructor(Context::class.java, iConnCls)
                c.isAccessible = true
                return c.newInstance(ShellContexts.systemContext(), conn)
            } catch (t: Throwable) {
                throw IllegalStateException("no usable UiAutomation constructor on SDK ${android.os.Build.VERSION.SDK_INT}", t)
            }
        }
        ctor.isAccessible = true
        return ctor.newInstance(looper, conn)
    }

    private fun connectWithRetry(ua: UiAutomation) {
        var last: Throwable? = null
        repeat(20) {
            try {
                invokeConnect(ua)
                return
            } catch (ite: InvocationTargetException) {
                val cause = ite.cause ?: ite
                last = cause
                if (cause.toString().contains("already registered")) {
                    Thread.sleep(250)   // Work thread, never the looper
                } else throw ite
            }
        }
        throw IllegalStateException("connect failed after retries", last)
    }

    private fun invokeConnect(ua: UiAutomation) {
        try {
            UiAutomation::class.java.getDeclaredMethod("connect", Int::class.javaPrimitiveType)
                .apply { isAccessible = true }.invoke(ua, 1)
            return
        } catch (_: NoSuchMethodException) {
        }
        UiAutomation::class.java.getDeclaredMethod("connect").apply { isAccessible = true }.invoke(ua)
    }

    private fun registerShutdownHook(ua: UiAutomation) {
        Runtime.getRuntime().addShutdownHook(Thread({
            try {
                UiAutomation::class.java.getDeclaredMethod("disconnect").apply { isAccessible = true }.invoke(ua)
            } catch (_: Throwable) {
            }
        }, "mobax-shutdown"))
    }

    /**
     * What `ActivityThread.main()` / `SystemServer.run()` set up that
     * app_process skips and we want: Bundles from other processes defused
     * (an unknown Parcelable reads as empty instead of throwing), and the
     * mainline modules' service managers, without which getSystemService()
     * for telephony, bluetooth, media session, stats … throws.
     */
    private fun initLikeActivityThreadMain() {
        try {
            Class.forName("android.os.BaseBundle")
                .getMethod("setShouldDefuse", Boolean::class.javaPrimitiveType).invoke(null, true)
        } catch (t: Throwable) {
            Logx.bootWarn("BaseBundle.setShouldDefuse failed", t)
        }
        if (android.os.Build.VERSION.SDK_INT < 30) return   // added in R
        try {
            Class.forName("android.app.ActivityThread").getDeclaredMethod("initializeMainlineModules")
                .apply { isAccessible = true }.invoke(null)
        } catch (t: Throwable) {
            Logx.bootWarn("initializeMainlineModules failed", t)
        }
    }

    /**
     * UiAutomation's callbacks run on the main looper: a message dispatched
     * over 100 ms, or delivered over 200 ms late, gets a logcat line (as in
     * system_server), so "events arrive late" has a culprit.
     */
    private fun logSlowMainLooper() {
        try {
            Looper::class.java.getMethod("setSlowLogThresholdMs", Long::class.javaPrimitiveType, Long::class.javaPrimitiveType)
                .invoke(Looper.getMainLooper(), 100L, 200L)
        } catch (t: Throwable) {
            Logx.bootWarn("Looper.setSlowLogThresholdMs failed", t)
        }
    }

    /** Our name in the debugger's process list; the signature varies by SDK. */
    private fun setDdmAppName(name: String) {
        try {
            val ddm = Class.forName("android.ddm.DdmHandleAppName")
            val s = String::class.java
            val i = Int::class.javaPrimitiveType
            val m = ddm.declaredMethods.filter { it.name == "setAppName" }
            m.find { it.parameterTypes.contentEquals(arrayOf(s, s, i)) }?.let { it.invoke(null, name, name, 0); return }
            m.find { it.parameterTypes.contentEquals(arrayOf(s, i)) }?.let { it.invoke(null, name, 0); return }
            m.find { it.parameterTypes.contentEquals(arrayOf(s)) }?.invoke(null, name)
        } catch (t: Throwable) {
            Logx.bootWarn("DdmHandleAppName.setAppName failed", t)
        }
    }

    private fun disableHiddenApiRestrictions() {
        try {
            val vmRuntime = Class.forName("dalvik.system.VMRuntime")
            val runtime = vmRuntime.getDeclaredMethod("getRuntime").invoke(null)
            vmRuntime.getDeclaredMethod("setHiddenApiExemptions", Array<String>::class.java)
                .invoke(runtime, arrayOf("L") as Any)
        } catch (t: Throwable) {
            Logx.bootWarn("could not lift hidden-api restrictions", t)
        }
    }
}
