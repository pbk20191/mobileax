package dev.mobileax.daemon

import android.os.ParcelFileDescriptor
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import java.io.FileDescriptor
import java.io.IOException
import kotlin.coroutines.resume
import kotlin.time.Duration.Companion.milliseconds

/**
 * Chunked-stream verbs backed by a pipe: dumpsys (a binder dump), logcat,
 * shell, monitor (shell-uid processes through [Shell]), plus clip_watch. Pipe read ends are watched on the main looper; each chunk
 * becomes one `[len][bytes]` frame, then `[len=0]`.
 */
internal class ProcStream(private val clip: Clipboard) {

    suspend fun dumpsys(c: Conn, tail: String?) {
        if (tail.isNullOrEmpty()) return c.errAndEnd("dumpsys-needs-service")
        val parts = tail.split(WS)
        val svc = parts[0]
        val args = parts.drop(1).toTypedArray()
        val b = try { withContext(Loop.Work) { Binders.service(svc) } } catch (t: Throwable) {
            if (t is kotlinx.coroutines.CancellationException) throw t
            // Our app uid may not look the service up (SELinux); the shell can.
            return runProcess(c, listOf("dumpsys") + parts, 64 * 1024, "dumpsys-spawn-failed:", exitTrailer = false)
        }
        val pipe = try { ParcelFileDescriptor.createPipe() } catch (e: IOException) {
            return c.errAndEnd("pipe-failed:${e.message}")
        }
        val (read, write) = pipe
        // The binder dump is synchronous and writes into the pipe, so it runs
        // on Work while the looper drains the read end.
        val pump = c.scope.async { try { pump(c, read.fileDescriptor, 64 * 1024) } finally { Io.unwatch(read.fileDescriptor); read.close() } }
        try {
            // Blocks until the client drains the pipe: keep it off Work.
            withContext(Loop.Blocking) { b.dump(write.fileDescriptor, args) }
        } catch (t: Throwable) {
            if (t is kotlinx.coroutines.CancellationException) { write.close(); throw t }
            c.errAndEnd("dump-threw:${t.javaClass.simpleName}:${t.message}")
            write.close()
            withTimeoutOrNull(2000.milliseconds) { pump.await() }
            return
        }
        write.close()
        pump.await()
        c.sendRaw(END)
    }

    suspend fun logcat(c: Conn, tail: String?) {
        val argv = listOf("/system/bin/logcat") + split(tail)
        runProcess(c, argv, 64 * 1024, "logcat-spawn-failed:", exitTrailer = false)
    }

    /**
     * `logcat_uid UID[,UID…] [ARGS…]`: logcat's lines of those uids only,
     * for Android 10 and older, whose logcat has no `--uid`: ARGS must ask
     * for `-v uid`, and grep keeps the lines whose uid column matches, so
     * the rest of the buffer never leaves the device.
     */
    suspend fun logcatUid(c: Conn, tail: String?) {
        val args = split(tail)
        val uids = args.firstOrNull()?.split(',')?.takeIf { u -> u.isNotEmpty() && u.all { it.isNotEmpty() && it.all(Char::isDigit) } }
            ?: return c.errAndEnd("logcat_uid-needs-uids")
        // MM-DD HH:MM:SS.mmm UID PID …
        val keep = "^[^ ]+ +[^ ]+ +(${uids.joinToString("|")}) "
        val cmd = Shell.argv(listOf("/system/bin/logcat") + args.drop(1)) +
            " | /system/bin/grep --line-buffered -E " + Shell.quote(keep)
        runProcess(c, cmd, 64 * 1024, "logcat-spawn-failed:", exitTrailer = false)
    }

    suspend fun monitor(c: Conn) =
        runProcess(c, listOf("/system/bin/am", "monitor"), 16 * 1024, "monitor-spawn-failed:", exitTrailer = false, openStdin = true)

    suspend fun shell(c: Conn, tail: String?) {
        if (tail.isNullOrEmpty()) return c.errAndEnd("shell-needs-argv")
        runProcess(c, split(tail), 64 * 1024, "spawn-failed:", exitTrailer = true)
    }

    /** Clipboard text as a StateFlow per poll interval, polled only while collected. */
    private val clipFlows = HashMap<Long, StateFlow<String?>>()

    private fun clipFlow(intervalMs: Long): StateFlow<String?> = clipFlows.getOrPut(intervalMs) {
        flow {
            while (true) {
                withContext(Loop.Work) { clip.currentText() }?.let { emit(it) }
                delay(intervalMs.milliseconds)
            }
        }.stateIn(Loop.scope, SharingStarted.WhileSubscribed(replayExpirationMillis = 0), null)
    }

    /** Pushes the clipboard text now and on every change; ends when the client does. */
    suspend fun clipWatch(c: Conn, intervalMs0: Long) {
        val intervalMs = if (intervalMs0 <= 0) 500 else intervalMs0
        clipFlow(intervalMs).filterNotNull().collect { c.send(it.toByteArray(Charsets.UTF_8)) }
    }

    /** [openStdin]: an open, empty stdin, which `am monitor` reads; others get
     *  EOF, since the framework holds stdout open while our stdin end is open. */
    private suspend fun runProcess(
        c: Conn, argv: List<String>, bufSize: Int, spawnErr: String, exitTrailer: Boolean, openStdin: Boolean = false,
    ) = runProcess(c, Shell.argv(argv), bufSize, spawnErr, exitTrailer, openStdin)

    /** [cmd] is one line of sh. */
    private suspend fun runProcess(
        c: Conn, cmd: String, bufSize: Int, spawnErr: String, exitTrailer: Boolean, openStdin: Boolean = false,
    ) {
        // Runs as the shell uid (Shell), stdout and stderr merged into one pipe
        // whose read end the looper drains.
        val p = try {
            withContext(Loop.Blocking) {
                Shell.open(cmd, mergeErr = true, input = if (openStdin) Shell.Input.OPEN else Shell.Input.NONE)
            }
        } catch (e: Exception) {
            return c.errAndEnd(spawnErr + e.message)
        }
        // The client leaving ends the pump even while the process is quiet
        // (am monitor between events): otherwise it would wait for output
        // that never comes, and kill() below would never run.
        val pumping = c.scope.async { pump(c, p.stdout.fileDescriptor, bufSize) }
        val hangup = c.scope.launch {
            try { c.awaitHangup() } catch (_: IOException) {}
            pumping.cancel()
            c.close()
        }
        try {
            try {
                pumping.await()
            } catch (e: kotlinx.coroutines.CancellationException) {
                if (!pumping.isCancelled) throw e
                return   // client gone
            }
            val code = p.exitCode()   // output ended: so has the script, and kill() leaves it be
            if (exitTrailer) c.send("__exit__ $code".toByteArray(Charsets.UTF_8))
            c.sendRaw(END)
        } finally {
            hangup.cancel()
            Io.unwatch(p.stdout.fileDescriptor)
            p.kill()   // stops it unless it ended
        }
    }

    /** Non-blocking read loop on the looper until EOF. */
    private suspend fun pump(c: Conn, fd: FileDescriptor, bufSize: Int) {
        MobaxServer.nonBlocking(fd)
        val buf = ByteArray(bufSize)
        while (true) {
            val n = try {
                Os.read(fd, buf, 0, buf.size)
            } catch (e: ErrnoException) {
                if (e.errno != OsConstants.EAGAIN) return
                awaitReadable(fd)
                continue
            }
            if (n <= 0) return
            c.send(buf.copyOf(n))
        }
    }

    private suspend fun awaitReadable(fd: FileDescriptor) = suspendCancellableCoroutine { k ->
        // Resume via post: resuming inline would re-watch this fd inside its own
        // callback, and the 0 returned here would then unregister it natively.
        Io.watch(fd, Io.IN or Io.ERR) { Loop.post { if (k.isActive) k.resume(Unit) }; 0 }
        // Unwatch before the caller closes the fd: a late unwatch could hit a
        // new fd that reused the number.
        k.invokeOnCancellation { Io.unwatch(fd) }
    }

    private suspend fun Conn.errAndEnd(tail: String) {
        send("ERR:$tail".toByteArray(Charsets.UTF_8))
        sendRaw(END)
    }

    private companion object {
        val WS = Regex("""\s+""")
        val END = byteArrayOf(0, 0, 0, 0)

        fun split(tail: String?): List<String> = if (tail.isNullOrEmpty()) emptyList() else tail.split(WS)
    }
}
