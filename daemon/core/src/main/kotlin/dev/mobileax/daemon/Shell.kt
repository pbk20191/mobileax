package dev.mobileax.daemon

import android.os.ParcelFileDescriptor
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.IOException
import java.io.OutputStream
import java.security.SecureRandom

/**
 * Commands run as the shell uid (dumpsys, settings, logcat, cp, …), one
 * line of sh each. How they start depends on the build ([openShell]): the
 * app_process daemon is the shell uid and spawns them itself; the
 * instrumentation daemon is an app uid and has the framework run them
 * through UiAutomation.
 */
object Shell {

    data class ShellResult(val out: String, val err: String, val exit: Int)

    /** What the command reads on stdin. */
    enum class Input {
        /** EOF. */
        NONE,
        /** Open and empty for as long as it runs (`am monitor` reads it). */
        OPEN,
        /** What the caller writes to [ShellProc.stdin], then closes. */
        DATA,
    }

    abstract class ShellProc {
        /** Only for [Input.DATA]; closing it starts or ends the command's input. */
        abstract val stdin: OutputStream?
        /** The command's stdout (and stderr with mergeErr). Throws for
         *  [Input.DATA] on SDK 29–33 until [stdin] is closed. */
        abstract val stdout: ParcelFileDescriptor
        /** Waits for the end; -1 if the code didn't come back (killed). */
        abstract suspend fun exitCode(): Int
        /** stderr (without mergeErr), after [exitCode]. */
        abstract val errOutput: String
        /** Stops it (and its children) unless it ended; frees what it holds. */
        abstract fun kill()
    }

    /** Single-quotes [s] for sh. */
    fun quote(s: String): String = "'" + s.replace("'", "'\\''") + "'"

    fun argv(args: List<String>): String = args.joinToString(" ") { quote(it) }

    /** Starts [cmd], one line of sh. [mergeErr] sends its stderr to stdout. Blocks briefly: call off the main looper. */
    fun open(cmd: String, mergeErr: Boolean = false, input: Input = Input.NONE): ShellProc =
        openShell(cmd, mergeErr, input)

    /**
     * Runs [cmd] to the end. Past [timeoutMs] it is killed (its pipes close,
     * so the blocked reads return) and exit is -1.
     */
    suspend fun run(cmd: String, timeoutMs: Long = 10_000): ShellResult {
        val p = withContext(Loop.Blocking) { open(cmd) }
        var timedOut = false
        val watchdog = Loop.scope.launch(Loop.Blocking) { delay(timeoutMs); timedOut = true; p.kill() }
        try {
            return coroutineScope {
                val out = async(Loop.Blocking) {
                    try {
                        ParcelFileDescriptor.AutoCloseInputStream(p.stdout).readBytes().toString(Charsets.UTF_8)
                    } catch (_: IOException) { "" }
                }
                val code = p.exitCode()
                val text = out.await()
                if (timedOut) ShellResult(text, "timeout after ${timeoutMs}ms", -1) else ShellResult(text, p.errOutput, code)
            }
        } finally {
            watchdog.cancel()
            p.kill()
        }
    }

    /** [run] from blocking code (callers already off the main looper). */
    fun runBlocking(cmd: String, timeoutMs: Long = 10_000): ShellResult =
        kotlinx.coroutines.runBlocking { run(cmd, timeoutMs) }

    private val random = SecureRandom()
    private val WS = Regex("""\s+""")

    fun nonce(): String {
        val b = ByteArray(16).also { random.nextBytes(it) }
        return b.joinToString("") { "%02x".format(it) }
    }

    /**
     * TERM to [pid] and every descendant, as the shell. The tree comes from
     * `ps` (toybox `pkill -P` matches nothing on some versions, e.g. SDK 29).
     * Its own commands get no watchdog, so this never recurses.
     */
    fun terminate(pid: Int) {
        if (pid <= 0) return
        Loop.scope.launch(Loop.Blocking) {
            try {
                val table = once("ps -A -o PID=,PPID=")
                val kids = HashMap<Int, MutableList<Int>>()
                for (line in table.lineSequence()) {
                    val f = line.trim().split(WS)
                    val child = f.getOrNull(0)?.toIntOrNull() ?: continue
                    val parent = f.getOrNull(1)?.toIntOrNull() ?: continue
                    kids.getOrPut(parent) { ArrayList() }.add(child)
                }
                val tree = ArrayList<Int>()
                val todo = ArrayDeque(listOf(pid))
                while (todo.isNotEmpty()) {
                    val x = todo.removeFirst()
                    if (x in tree) continue
                    tree.add(x)
                    kids[x]?.let { todo.addAll(it) }
                }
                once("kill -TERM ${tree.joinToString(" ")}")
            } catch (_: Throwable) {}
        }
    }

    /** One short command's stdout, no watchdog (for [terminate]). */
    private suspend fun once(cmd: String): String {
        val p = open(cmd)
        try {
            val out = ParcelFileDescriptor.AutoCloseInputStream(p.stdout).use { it.readBytes() }.toString(Charsets.UTF_8)
            p.exitCode()
            return out
        } finally { p.kill() }
    }
}
