package dev.mobileax.daemon

import android.os.ParcelFileDescriptor
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import java.io.BufferedInputStream
import java.io.IOException
import java.io.InputStream
import kotlin.time.Duration.Companion.milliseconds

/**
 * Commands that need the shell uid (dumpsys, settings, logcat, …): the daemon
 * itself is an app uid. They run through UiAutomation.executeShellCommandRwe,
 * which execs as shell; UiDevice.executeShellCommand only returns stdout, so
 * it can't report stderr or the exit code.
 *
 * executeShellCommand* splits its argument on whitespace and honours no
 * quoting, so it only ever starts `sh`; the script goes in on stdin. It
 * first reports the shell's pid on stderr (`PID_MARK$$`), so [ShellProc.kill]
 * can stop it: the framework keeps the process alive when only our pipe
 * ends close. It ends with `echo EXIT_MARK$? >&2`, the exit code.
 */
internal object Shell {
    const val EXIT_MARK = "__MOBAX_EXIT:"
    const val PID_MARK = "__MOBAX_PID:"

    data class ShellResult(val out: String, val err: String, val exit: Int)

    /** A running script. stdout (and stderr when merged) is the output;
     *  stderr carries the exit marker. */
    class ShellProc(
        val stdin: ParcelFileDescriptor?,
        val stdout: ParcelFileDescriptor,
        private val stderr: InputStream,
        private val pid: Int,
    ) {
        @Volatile private var errText = ""
        @Volatile private var ended = false

        /** Waits for the script to end; what it wrote to stderr (marker
         *  removed) is [errOutput] afterwards. -1 if no marker came back. */
        suspend fun exitCode(): Int = withContext(Loop.Blocking) {
            val text = try {
                stderr.use { it.readBytes() }.toString(Charsets.UTF_8)
            } catch (_: IOException) { "" }
            ended = true
            val at = text.lastIndexOf(EXIT_MARK)
            if (at < 0) { errText = text; return@withContext -1 }
            errText = text.substring(0, at)
            text.substring(at + EXIT_MARK.length).substringBefore('\n').trim().toIntOrNull() ?: -1
        }

        val errOutput: String get() = errText

        /** Closes our ends and, unless it already ended, terminates the
         *  script and its children (as shell, from another script). */
        fun kill() {
            try { stdin?.close() } catch (_: IOException) {}
            try { stdout.close() } catch (_: IOException) {}
            if (!ended && pid > 0) {
                Loop.scope.launch(Loop.Blocking) { try { run("pkill -TERM -P $pid; kill -TERM $pid", 5_000) } catch (_: Throwable) {} }
            }
            try { stderr.close() } catch (_: IOException) {}
        }
    }

    /** Single-quotes [s] for sh. */
    fun quote(s: String): String = "'" + s.replace("'", "'\\''") + "'"

    fun argv(args: List<String>): String = args.joinToString(" ") { quote(it) }

    /**
     * Starts [cmd], one line of sh. [mergeErr] sends the command's stderr to
     * stdout, as the old redirectErrorStream(true) did. With [keepStdin] the
     * command's stdin stays open as [ShellProc.stdin] (data to write, or just
     * an open, empty stdin, as `am monitor` needs); otherwise it reads EOF.
     */
    fun open(cmd: String, mergeErr: Boolean = false, keepStdin: Boolean = false): ShellProc {
        val (out, inp, errFd) = ua.executeShellCommandRwe("sh")
        val body = if (mergeErr) "{ $cmd; } 2>&1" else cmd
        // sh reads its script line by line and the command inherits that same
        // stdin, so the whole script is one line: the command's input is only
        // what follows it. The trailing exit keeps sh from running that input
        // as script when the command stops early (a missing directory, say).
        val script = "echo $PID_MARK\$\$ >&2; $body; echo $EXIT_MARK\$? >&2; exit\n".toByteArray(Charsets.UTF_8)
        if (!keepStdin) {
            ParcelFileDescriptor.AutoCloseOutputStream(inp).use { it.write(script) }
        } else {
            ParcelFileDescriptor.AutoCloseOutputStream(inp.dup()).use { it.write(script) }
        }
        val err = BufferedInputStream(ParcelFileDescriptor.AutoCloseInputStream(errFd))
        return ShellProc(if (keepStdin) inp else null, out, err, readPid(err))
    }

    /** The first stderr line, `PID_MARK<pid>`; 0 if it doesn't come. */
    private fun readPid(err: InputStream): Int {
        val line = StringBuilder()
        while (true) {
            val b = err.read()
            if (b < 0 || b == '\n'.code) break
            line.append(b.toChar())
        }
        return line.removePrefix(PID_MARK).trim().toString().toIntOrNull() ?: 0
    }

    /** Runs [cmd] to the end. On timeout the script is killed and exit is -1. */
    suspend fun run(cmd: String, timeoutMs: Long = 10_000): ShellResult {
        val p = withContext(Loop.Blocking) { open(cmd) }
        val r = withTimeoutOrNull(timeoutMs.milliseconds) {
            coroutineScope {
                val out = async(Loop.Blocking) {
                    ParcelFileDescriptor.AutoCloseInputStream(p.stdout).readBytes().toString(Charsets.UTF_8)
                }
                val code = p.exitCode()
                ShellResult(out.await(), p.errOutput, code)
            }
        }
        if (r == null) p.kill()
        return r ?: ShellResult("", "timeout after ${timeoutMs}ms", -1)
    }

    /** [run] from blocking code (callers already off the main looper). */
    fun runBlocking(cmd: String, timeoutMs: Long = 10_000): ShellResult =
        kotlinx.coroutines.runBlocking { run(cmd, timeoutMs) }
}
