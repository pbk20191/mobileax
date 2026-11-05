package dev.mobileax.daemon

import android.content.Context
import android.content.Intent
import android.content.IntentSender
import android.content.pm.PackageInstaller
import android.os.Binder
import android.os.IBinder
import android.os.Parcel
import android.os.ParcelFileDescriptor
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import java.io.File
import java.io.IOException
import java.io.OutputStream
import kotlin.time.Duration.Companion.milliseconds

/**
 * Receives a PackageInstaller status through a local Binder implementing only
 * `IIntentSender.send` (transaction 1, stable across API levels), so the
 * daemon needs no package name or BroadcastReceiver.
 */
class StatusReceiver : Binder() {
    private val result = CompletableDeferred<Intent>()

    val intentSender: IntentSender = IntentSender::class.java
        .getDeclaredConstructor(IBinder::class.java).apply { isAccessible = true }.newInstance(this)

    /** null on timeout. */
    suspend fun await(timeoutMs: Long): Intent? = withTimeoutOrNull(timeoutMs.milliseconds) { result.await() }

    override fun onTransact(code: Int, data: Parcel, reply: Parcel?, flags: Int): Boolean {
        if (code != 1) return super.onTransact(code, data, reply, flags)
        try { data.enforceInterface("android.content.IIntentSender") } catch (_: Throwable) {}
        var intent: Intent? = null
        try {
            data.readInt()   // sendCode
            if (data.readInt() != 0) intent = Intent.CREATOR.createFromParcel(data)
        } catch (_: Throwable) {}
        result.complete(intent ?: Intent())
        reply?.writeNoException()
        return true
    }
}

/**
 * pull / push / install / install_multi / pm_uninstall. Wire format: chunked
 * `[len][bytes]… [len=0]` in either direction, then one reply frame. Socket
 * I/O stays on the looper; file and PackageInstaller I/O runs on Work.
 */
internal class Transfers(private val ctx: Context, private val installer: Installer) {

    // ── chunk streams ───────────────────────────────────────────────────

    /** Reads chunk frames until len=0, handing each to [sink] on Work. */
    private suspend fun Conn.copyChunks(sink: OutputStream): Long {
        var total = 0L
        while (true) {
            val n = readInt()
            if (n == 0) return total
            if (n !in 0..MAX_CHUNK) throw IOException("bad-chunk-size:$n")
            val chunk = readFully(n)
            withContext(Loop.Work) { sink.write(chunk) }
            total += n
        }
    }

    /** Best-effort discard of an inbound chunk stream after a fatal error. */
    private suspend fun Conn.drainChunks() {
        try {
            while (true) {
                val n = readInt()
                if (n !in 1..MAX_CHUNK) return
                skip(n)
            }
        } catch (_: IOException) {
        }
    }

    /** Streams [path] as the shell (our app uid can't read most paths). A
     *  file that can't be opened fails with ERR before any bytes. */
    suspend fun pull(c: Conn, path: String?) {
        if (path == null) return c.errAndEnd("pull-needs-path")
        val q = Shell.quote(path)
        val p = try {
            Shell.start("( [ -e $q ] || exit $NOT_FOUND; [ -d $q ] && exit $IS_DIR; exec cat $q )")
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Exception) {
            return c.errAndEnd("pull-failed:${e.message}")
        }
        var sent = 0L
        try {
            val input = ParcelFileDescriptor.AutoCloseInputStream(p.stdout)
            while (true) {
                val buf = ByteArray(CHUNK)
                val n = withContext(Loop.Blocking) { input.read(buf) }
                if (n <= 0) break
                c.send(buf, 0, n)
                sent += n
            }
            val code = p.exitCode()
            when {
                // Clients read ERR only as the first frame, so a failure
                // after data can only end the stream.
                code == 0 || sent > 0 -> c.sendRaw(END)
                code == NOT_FOUND -> c.errAndEnd("not-found:$path")
                code == IS_DIR -> c.errAndEnd("is-directory:$path")
                else -> c.errAndEnd("read-failed:" + p.errOutput.trim().ifEmpty { "exit $code" })
            }
        } catch (e: IOException) {
            if (c.closed) throw e
            Logx.w("pull failed", e)
            if (sent == 0L) c.send(("ERR:read-failed:" + e.message).toByteArray(Charsets.UTF_8))
            c.sendRaw(END)
        } finally {
            p.kill()
        }
    }

    /** Writes the inbound chunks to [path] as the shell. */
    suspend fun push(c: Conn, path: String, mode: Int, expectedSize: Long): ByteArray {
        val q = Shell.quote(path)
        val parent = File(path).parent
        val dirCheck = if (parent != null) "[ -d ${Shell.quote(parent)} ] || exit $NOT_FOUND; " else ""
        val chmod = if (mode > 0) " && chmod ${Integer.toOctalString(mode)} $q" else ""
        val p = try {
            Shell.start("( ${dirCheck}cat > $q$chmod )", input = Shell.Input.DATA)
        } catch (e: Exception) {
            c.drainChunks()
            return err("write-failed:${e.message}")
        }
        val sink = p.stdin!!
        var received = 0L
        var writeErr: IOException? = null
        try {
            received = c.copyChunks(sink)
        } catch (e: IOException) {
            if (c.closed) { p.kill(); throw e }
            writeErr = e   // the script stopped reading (it failed): drop the rest
            c.drainChunks()
        }
        withContext(Loop.Blocking) { try { sink.close() } catch (_: IOException) {} }
        val code = p.exitCode()
        p.kill()
        return when {
            code == NOT_FOUND -> err("parent-missing:$parent")
            code != 0 -> err("write-failed:" + p.errOutput.trim().ifEmpty { writeErr?.message ?: "exit $code" })
            expectedSize > 0 && received != expectedSize -> err("size-mismatch:got=$received:want=$expectedSize")
            else -> ok("ok bytes=$received")
        }
    }

    suspend fun install(c: Conn, size: Long, reinstall: Boolean, grantAll: Boolean): ByteArray =
        installSession(c, if (size > 0) longArrayOf(size) else longArrayOf(0), reinstall, grantAll, multi = false)

    suspend fun installMulti(c: Conn, sizes: LongArray, reinstall: Boolean, grantAll: Boolean): ByteArray {
        if (sizes.isEmpty()) { c.drainChunks(); return err("install-multi-no-sizes") }
        return installSession(c, sizes, reinstall, grantAll, multi = true)
    }

    private suspend fun installSession(c: Conn, sizes: LongArray, reinstall: Boolean, grantAll: Boolean, multi: Boolean): ByteArray {
        val pi = withContext(Loop.Work) { ctx.packageManager.packageInstaller }
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
        val total = sizes.sum()
        if (total > 0) params.setSize(total)
        if (reinstall) Installer.trySetInstallFlags(params, 0x2)   // INSTALL_REPLACE_EXISTING
        // USER_ACTION_NOT_REQUIRED on API 31+; ignored before.
        Installer.trySetRequireUserAction(params, 2)

        val sessionId = try {
            withContext(Loop.Work) { pi.createSession(params) }
        } catch (e: IOException) {
            // Keep the socket in sync.
            if (multi) repeat(sizes.size) { c.drainChunks() } else c.drainChunks()
            return err("createSession-failed:${e.message}")
        }

        var session: PackageInstaller.Session? = null
        var written = 0L
        try {
            session = withContext(Loop.Work) { pi.openSession(sessionId) }
            for (i in sizes.indices) {
                val slot = if (i == 0) "base.apk" else "split_$i.apk"
                val want = sizes[i]
                val apk = withContext(Loop.Work) { session.openWrite(slot, 0, if (want > 0) want else -1) }
                val got = try {
                    c.copyChunks(apk).also { withContext(Loop.Work) { session.fsync(apk) } }
                } finally {
                    withContext(NonCancellable + Loop.Work) { try { apk.close() } catch (_: IOException) {} }
                }
                if (want > 0 && got != want) {
                    withContext(Loop.Work) { session.abandon() }
                    return err(if (multi) "split-$i-size-mismatch:got=$got:want=$want" else "size-mismatch:got=$got:want=$want")
                }
                written += got
            }

            val status = StatusReceiver()
            withContext(Loop.Work) { session.commit(status.intentSender) }
            val result = status.await(if (multi) 180_000 else 120_000) ?: return err("install-timeout")
            val code = result.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
            val pkg = result.getStringExtra(PackageInstaller.EXTRA_PACKAGE_NAME)
            val msg = result.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)
            if (code == PackageInstaller.STATUS_SUCCESS) {
                if (grantAll && pkg != null) withContext(Loop.Work) { installer.grantAllRequested(pkg) }
                return ok("ok package=${pkg ?: "?"}" + (if (multi) " splits=${sizes.size}" else "") + " bytes=$written")
            }
            return err("install-failed:status=$code pkg=$pkg msg=$msg")
        } catch (t: Throwable) {
            val s = session
            try { if (s != null) withContext(NonCancellable + Loop.Work) { s.abandon() } } catch (_: Throwable) {}
            if (t is kotlinx.coroutines.CancellationException) throw t
            return err((if (multi) "install-multi-threw:" else "install-threw:") + t.javaClass.simpleName + ":" + t.message)
        } finally {
            val s = session
            if (s != null) try { s.close() } catch (_: Throwable) {}
        }
    }

    suspend fun uninstall(pkg: String): ByteArray = try {
        val pi = withContext(Loop.Work) { ctx.packageManager.packageInstaller }
        val status = StatusReceiver()
        withContext(Loop.Work) { pi.uninstall(pkg, status.intentSender) }
        val result = status.await(60_000)
        if (result == null) err("uninstall-timeout")
        else {
            val code = result.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
            val msg = result.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)
            if (code == PackageInstaller.STATUS_SUCCESS) ok("ok package=$pkg")
            else err("uninstall-failed:status=$code msg=$msg")
        }
    } catch (t: Throwable) {
        if (t is kotlinx.coroutines.CancellationException) throw t
        err("uninstall-threw:${t.javaClass.simpleName}:${t.message}")
    }

    private suspend fun Conn.errAndEnd(tail: String) {
        send(err(tail))
        sendRaw(END)
    }

    private companion object {
        const val CHUNK = 256 * 1024
        /** Exit codes of the pull/push scripts' own checks. */
        const val NOT_FOUND = 3
        const val IS_DIR = 4
        const val MAX_CHUNK = 8 * 1024 * 1024
        val END = byteArrayOf(0, 0, 0, 0)
        fun ok(s: String) = s.toByteArray(Charsets.UTF_8)
        fun err(tail: String) = "ERR:$tail".toByteArray(Charsets.UTF_8)
    }
}
