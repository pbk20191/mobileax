package dev.mobileax.daemon

import android.app.UiAutomation
import android.content.Context
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlin.concurrent.Volatile

/**
 * What differs between the two daemons built on this module: the
 * app_process one (daemon/shell, the shell uid) and the instrumentation
 * (daemon/instr, an app uid with the shell's permissions adopted). Each
 * entry point sets [current] before anything else runs.
 */
interface Host {
    /** The Context binder calls and services go through; its package belongs to our uid. */
    val context: Context

    /** The callingPackage for binder calls: it must belong to our uid. */
    val callerPackage: String

    /** Our own installed package, hidden from package lists; null when there is none. */
    val selfPackage: String?

    /** A startup line for /data/local/tmp/mobax.log. */
    fun startupLine(line: String)

    /**
     * The Instrumentation UiAutomator's UiDevice runs on, handing it [ua]
     * (UiDevice is one per process). The instrumentation's own; the
     * app_process daemon, which has none, makes a stand-in.
     */
    fun instrumentation(ua: UiAutomation): android.app.Instrumentation

    /** Starts [cmd], one line of sh, as the shell uid. Blocks briefly: off the main looper. */
    fun openShell(cmd: String, mergeErr: Boolean, input: Shell.Input): Shell.ShellProc

    /**
     * Ends the daemon on purpose (`quit`, no clients left), with [reason]
     * in the log. The instrumentation finishes, so `am instrument` reports
     * a result instead of a crash; app_process exits. Main thread; may
     * return before the process is gone.
     */
    fun exit(reason: String)

    companion object {
        @Volatile var current: Host? = null

        internal val it: Host get() = checkNotNull(current) { "Host.current not set" }
    }
}

internal val ctx: Context get() = Host.it.context
internal val callerPackage: String get() = Host.it.callerPackage
internal val selfPackage: String? get() = Host.it.selfPackage

/** Before the host is set (the first startup steps), stdout. */
internal fun startupLine(line: String) = Host.current?.startupLine(line) ?: println(line)

internal fun openShell(cmd: String, mergeErr: Boolean, input: Shell.Input): Shell.ShellProc =
    Host.it.openShell(cmd, mergeErr, input)

/**
 * Listens on [port] at once and answers verbs once [connect] has handed
 * over a Context and a connected UiAutomation (on a Work thread) and the
 * handlers are built. [onInitFailed] runs after a failed init has been
 * logged and reported to waiting clients. Call on the main thread, after
 * [Loop.init].
 */
fun serve(port: Int, connect: suspend () -> Pair<Context, UiAutomation>, onInitFailed: (Throwable) -> Unit) {
    val ready = CompletableDeferred<Server>()
    lateinit var server: MobaxServer
    // `quit` exits the process right after (Dispatch).
    server = MobaxServer(port, Dispatch(ready, onQuit = { server.close() }))
    server.start()
    Lease.start { reason -> server.close(); Host.it.exit(reason) }
    Loop.scope.launch {
        try {
            val built = withContext(Loop.Work) {
                val (context, ua) = connect()
                buildServer(context, ua)
            }
            ready.complete(built)
            Logx.boot("mobaxd ready (sdk=${android.os.Build.VERSION.SDK_INT})")
        } catch (t: Throwable) {
            Logx.fatal("init failed", t)
            ready.completeExceptionally(t)   // `ready` callers get the failure now
            onInitFailed(t)
        }
    }
}
