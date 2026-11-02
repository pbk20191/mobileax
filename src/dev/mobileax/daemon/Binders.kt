package dev.mobileax.daemon

import android.app.Application
import android.app.Instrumentation
import android.content.Context
import android.content.pm.ApplicationInfo
import android.os.IBinder
import android.os.Looper
import android.os.Process

/**
 * Central pool for Binder + system-Context lookups.
 * 
 * Everything goes through reflection because we want a single jar to build
 * against the public SDK while still calling hidden APIs at runtime
 * (ServiceManager, IPackageManager$Stub, IActivityManager$Stub).
 * 
 * [Main.disableHiddenApiRestrictions] runs at process start so all of
 * the reflective lookups below succeed without throwing.
 */
internal object Binders {
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

    @JvmStatic
    fun systemContext(): Context = systemContextLazy

    /**
     * The daemon's Application, as com.android.shell: the package of the uid
     * we run as. systemMain() already made one, but on the "android" package,
     * so its op package and AttributionSource name a package uid 2000 doesn't
     * own and the services that check (virtual displays, app ops) refuse it.
     * Installed as ActivityThread.mInitialApplication so currentOpPackageName()
     * and currentAttributionSource() agree. Falls back to the system context.
     */
    private val appContextLazy: Context by lazy {
        try {
            shellApplication()
        } catch (t: Throwable) {
            Logx.bootWarn("shell Application failed; using the system context", t)
            systemContextLazy
        }
    }

    @JvmStatic
    fun appContext(): Context = appContextLazy

    private fun shellApplication(): Application {
        val sys = systemContextLazy
        val atCls = Class.forName("android.app.ActivityThread")
        val at = atCls.getMethod("currentActivityThread").invoke(null)
        val info = sys.packageManager.getApplicationInfo("com.android.shell", 0)
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

    @JvmStatic
    fun service(name: String?): IBinder {
        try {
            val sm = Class.forName("android.os.ServiceManager")
            val get = sm.getMethod("getService", String::class.java)
            val b = get.invoke(null, name) as IBinder
            return b
        } catch (t: Throwable) {
            throw RuntimeException("ServiceManager.getService($name) failed", t)
        }
    }

    /** Resolve `<stubFqcn>.asInterface(IBinder)` and wrap the given binder.  */
    @JvmStatic
    fun asInterface(stubFqcn: String, b: IBinder?): Any? {
        try {
            val stub = Class.forName(stubFqcn)
            val m = stub.getMethod("asInterface", IBinder::class.java)
            return m.invoke(null, b)
        } catch (t: Throwable) {
            throw RuntimeException("asInterface failed for $stubFqcn", t)
        }
    }

    @JvmStatic
    fun iPackageManager(): Any? {
        return asInterface($$"android.content.pm.IPackageManager$Stub", service("package"))
    }

    /**
     * Permission management split out from IPackageManager around API 30.
     * Falls back to iPackageManager() so callers can try whichever interface
     * exposes the method they need.
     */
    @JvmStatic
    fun iPermissionManagerOrNull(): Any? {
        return try {
            asInterface(
                $$"android.permission.IPermissionManager$Stub", service("permissionmgr")
            )
        } catch (_: Throwable) {
            null
        }
    }

    @JvmStatic
    fun iActivityManager(): Any? {
        return asInterface($$"android.app.IActivityManager$Stub", service("activity"))
    }

    /** `UserHandle.myUserId()` is @hide; reach it via reflection.  */
    @JvmStatic
    fun myUserId(): Int {
        try {
            val m = Class.forName("android.os.UserHandle").getMethod("myUserId")
            return (m.invoke(null) as Int?)!!
        } catch (_: Throwable) {
            // Decode user-id directly from our UID (userId * 100000 + appId).
            return Process.myUid() / 100000
        }
    }

    /** API 30+. May be unavailable on older surfaces — callers handle null.  */
    @JvmStatic
    fun iActivityTaskManagerOrNull(): Any? {
        return try {
            asInterface(
                $$"android.app.IActivityTaskManager$Stub", service("activity_task")
            )
        } catch (_: Throwable) {
            null
        }
    }
}
