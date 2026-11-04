package dev.mobileax.daemon

import android.os.Build
import android.os.ParcelFileDescriptor
import dev.mobileax.daemon.Shell.Input
import dev.mobileax.daemon.Shell.ShellProc
import dev.mobileax.daemon.Shell.nonce
import dev.mobileax.daemon.Shell.terminate
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.BufferedInputStream
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import kotlin.concurrent.Volatile

/**
 * [Shell] for the instrumentation daemon, an app uid: the framework runs
 * each command as the shell through UiAutomation, two ways:
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
internal object UaShell {
    /** Set from `am instrument -e shell file`, to exercise the 29–33 path on a newer device. */
    @Volatile var forceFiles = false

    val useFiles: Boolean get() = forceFiles || Build.VERSION.SDK_INT < 34

    val dir: File by lazy {
        (ctx.getExternalFilesDir("shell") ?: throw IOException("no external files dir")).also { d ->
            d.mkdirs()
            d.listFiles()?.forEach { it.delete() }   // left by a daemon that died mid-command
        }
    }
}

internal fun openShell(cmd: String, mergeErr: Boolean, input: Input): ShellProc =
    if (UaShell.useFiles) FileProc(cmd, mergeErr, input) else RweProc(cmd, mergeErr, input)

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

private class FileProc(private val cmd: String, private val mergeErr: Boolean, private val input: Input) : ShellProc() {
    private val n = nonce()
    private fun f(ext: String) = File(UaShell.dir, "$n.$ext")
    private val script = f("sh"); private val errF = f("err"); private val codeF = f("code")
    private val pidF = f("pid"); private val inF = f("in")
    @Volatile private var out: ParcelFileDescriptor? = null
    @Volatile private var errText = ""
    @Volatile private var ended = false
    @Volatile private var killed = false
    override val stdin: OutputStream?

    init {
        val p = UaShell.dir.absolutePath
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
