package dev.mobileax.daemon

import android.os.Build
import android.os.ParcelFileDescriptor
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.BufferedInputStream
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.security.SecureRandom
import kotlin.concurrent.Volatile

/**
 * Commands that need the shell uid (dumpsys, settings, logcat, cp, …): the
 * daemon itself is an app uid. The framework runs them as the shell through
 * UiAutomation, two ways:
 *
 *  - SDK 34+: executeShellCommandRwe (public there) gives stdin, stdout and
 *    stderr pipes. It splits its argument on whitespace with no quoting, so
 *    it only starts `sh` and the script goes in on stdin, one line.
 *  - SDK 29–33: executeShellCommand (stdout only). The script is a file in our
 *    app-specific external directory, which we may write and the shell may
 *    read and write; stdin data, stderr, the exit code and the pid travel as
 *    files beside it. It runs as `sh <path>`: no whitespace, so no quoting.
 *
 * Either way the shell's pid and exit code come back tagged with a random
 * per-call nonce, so a command can't forge or collide with them.
 * UiDevice.executeShellCommand is stdout only, so it isn't used.
 */
internal object Shell {

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

    /** Set from `am instrument -e shell file`, to exercise the 29–33 path on a newer device. */
    @Volatile var forceFiles = false

    private val useFiles: Boolean get() = forceFiles || Build.VERSION.SDK_INT < 34

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
        if (useFiles) FileProc(cmd, mergeErr, input) else RweProc(cmd, mergeErr, input)

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

    private fun nonce(): String {
        val b = ByteArray(16).also { random.nextBytes(it) }
        return b.joinToString("") { "%02x".format(it) }
    }

    /**
     * TERM to [pid] and every descendant, as the shell. The tree comes from
     * `ps` (toybox `pkill -P` matches nothing on some versions, e.g. SDK 29).
     * Its own commands get no watchdog, so this never recurses.
     */
    private fun terminate(pid: Int) {
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

    // ── SDK 34+: three pipes ────────────────────────────────────────────

    private class RweProc(cmd: String, mergeErr: Boolean, input: Input) : ShellProc() {
        private val tag = "__MOBAX_${nonce()}_"
        private val inp: ParcelFileDescriptor?
        override val stdout: ParcelFileDescriptor
        private val err: InputStream
        private val pid: Int
        @Volatile private var errText = ""
        @Volatile private var ended = false
        override val stdin: OutputStream?

        init {
            val (out, i, e) = ua.executeShellCommandRwe("sh")
            stdout = out
            val body = if (mergeErr) "{ $cmd; } 2>&1" else cmd
            // One line: sh reads its script line by line and the command
            // inherits that stdin, so its input is only what follows. The
            // trailing exit keeps sh from running that input as script when
            // the command stops early.
            val script = "echo ${tag}PID:\$\$ >&2; $body; echo ${tag}EXIT:\$? >&2; exit\n".toByteArray(Charsets.UTF_8)
            if (input == Input.NONE) {
                ParcelFileDescriptor.AutoCloseOutputStream(i).use { it.write(script) }
                inp = null
            } else {
                ParcelFileDescriptor.AutoCloseOutputStream(i.dup()).use { it.write(script) }
                inp = i
            }
            stdin = if (input == Input.DATA) ParcelFileDescriptor.AutoCloseOutputStream(inp) else null
            err = BufferedInputStream(ParcelFileDescriptor.AutoCloseInputStream(e))
            pid = readPid()
        }

        /** The first stderr line is ours: `<tag>PID:<pid>`. */
        private fun readPid(): Int {
            val line = StringBuilder()
            while (true) {
                val b = try { err.read() } catch (_: IOException) { -1 }
                if (b < 0 || b == '\n'.code) break
                line.append(b.toChar())
            }
            val s = line.toString()
            return if (s.startsWith("${tag}PID:")) s.removePrefix("${tag}PID:").trim().toIntOrNull() ?: 0 else 0
        }

        override suspend fun exitCode(): Int = withContext(Loop.Blocking) {
            val text = try { err.use { it.readBytes() }.toString(Charsets.UTF_8) } catch (_: IOException) { "" }
            ended = true
            val at = text.lastIndexOf("${tag}EXIT:")
            if (at < 0) { errText = text; return@withContext -1 }
            errText = text.substring(0, at) + text.substring(at).substringAfter('\n', "")
            text.substring(at + tag.length + "EXIT:".length).substringBefore('\n').trim().toIntOrNull() ?: -1
        }

        override val errOutput: String get() = errText

        override fun kill() {
            try { inp?.close() } catch (_: IOException) {}
            try { stdout.close() } catch (_: IOException) {}
            if (!ended) terminate(pid)
            try { err.close() } catch (_: IOException) {}
        }
    }

    // ── SDK 29–33: stdout pipe, the rest as files ───────────────────────

    private val dir: File by lazy {
        (ctx.getExternalFilesDir("shell") ?: throw IOException("no external files dir")).also { d ->
            d.mkdirs()
            d.listFiles()?.forEach { it.delete() }   // left by a daemon that died mid-command
        }
    }

    private class FileProc(private val cmd: String, private val mergeErr: Boolean, private val input: Input) : ShellProc() {
        private val n = nonce()
        private fun f(ext: String) = File(dir, "$n.$ext")
        private val script = f("sh"); private val errF = f("err"); private val codeF = f("code")
        private val pidF = f("pid"); private val inF = f("in")
        @Volatile private var out: ParcelFileDescriptor? = null
        @Volatile private var errText = ""
        @Volatile private var ended = false
        @Volatile private var killed = false
        override val stdin: OutputStream?

        init {
            val p = dir.absolutePath
            require(!p.any { it.isWhitespace() }) { "shell dir has whitespace: $p" }
            val redirIn = when (input) {
                Input.NONE -> "< /dev/null"
                Input.DATA -> "< ${inF.absolutePath}"
                Input.OPEN -> ""
            }
            val redirErr = if (mergeErr) "2>&1" else "2> ${errF.absolutePath}"
            val body = if (input == Input.OPEN) "sleep 2147483647 | { $cmd\n} $redirErr" else "{ $cmd\n} $redirIn $redirErr"
            script.writeText("echo \$\$ > ${pidF.absolutePath}\n$body\necho \$? > ${codeF.absolutePath}.t && mv ${codeF.absolutePath}.t ${codeF.absolutePath}\n")
            if (input == Input.DATA) {
                // The command starts once the caller has written all of it.
                stdin = object : FileOutputStream(inF) {
                    private var closed = false
                    override fun close() {
                        super.close()
                        // Not when kill() closes it: partial input never runs.
                        if (!closed && !killed) { closed = true; launch() }
                    }
                }
            } else {
                stdin = null
                launch()
            }
        }

        private fun launch() {
            out = ua.executeShellCommand("sh ${script.absolutePath}")
        }

        override val stdout: ParcelFileDescriptor get() = out ?: throw IOException("command not started: close stdin first")

        override suspend fun exitCode(): Int = withContext(Loop.Blocking) {
            // The code file appears (atomically, by rename) when the script ends.
            while (!codeF.exists() && !killed) delay(10)
            ended = true
            errText = try { if (errF.exists()) errF.readText() else "" } catch (_: IOException) { "" }
            try { codeF.readText().trim().toIntOrNull() ?: -1 } catch (_: IOException) { -1 }
        }

        override val errOutput: String get() = errText

        override fun kill() {
            killed = true
            try { stdin?.close() } catch (_: IOException) {}
            try { out?.close() } catch (_: IOException) {}
            if (!ended && !codeF.exists()) {
                val pid = try { pidF.readText().trim().toIntOrNull() ?: 0 } catch (_: IOException) { 0 }
                terminate(pid)
            }
            Loop.scope.launch(Loop.Blocking) {
                delay(2_000)   // after the script's last write
                for (x in listOf(script, errF, codeF, pidF, inF, File("${codeF.absolutePath}.t"))) x.delete()
            }
        }
    }
}
