package dev.mobileax.daemon

import android.app.UiAutomation
import android.content.Context
import android.graphics.Point
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityWindowInfo
import androidx.test.uiautomator.By
import androidx.test.uiautomator.BySelector
import androidx.test.uiautomator.ByWindowSelector
import androidx.test.uiautomator.Configurator
import androidx.test.uiautomator.Direction
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.UiObject2
import androidx.test.uiautomator.Until
import java.util.regex.Pattern

/**
 * `by:` queries: UiAutomator's By selectors, run live on the device.
 *
 * The query language (after `by:`), space-separated terms, all of which
 * must hold:
 *
 *   text=V  text~=V  text^=V  text$=V  text=/re/i     (also desc, hint, res, class, pkg)
 *   clickable  checked  !enabled  …                    state flags; `!` negates
 *   has(Q)  child(Q)  parent(Q)  in(Q)                 descendant, child, parent, ancestor
 *   depth=N
 *   window(pkg=… title~=… type=app|ime|system|overlay active focused display=N)
 *
 * `res=title` matches any package's `:id/title`; `class=Button` any
 * package's Button. Values with spaces go in double quotes.
 *
 * Wire: options first, then `q=` and the query to the end of the line:
 *
 *   by_find q=Q                         → {"nodes":[…]} (dump node fields)
 *   by_wait [timeout_ms=N] [gone=1] q=Q → the first node, or ERR:TIMEOUT
 *   by_tap [timeout_ms=N] [long=1] [nth=N] [new_window_ms=N] q=Q
 *   by_swipe dir=up|down|left|right [pct=0.8] [fling=1] [nth=N] q=Q   finger direction; `end` when nothing is left
 *   by_pinch in|out [pct=0.8] [nth=N] q=Q
 *   by_drag to=X,Y [speed=N] [nth=N] q=Q
 *
 * UiDevice runs on [Host.instrumentation]: the instrumentation's own, or in
 * the app_process daemon a stand-in that hands it our UiAutomation.
 */
internal class ByEngine(private val ua: UiAutomation, private val ctx: Context, private val events: UiEvents) {

    private val device: UiDevice by lazy {
        Configurator.getInstance().apply {
            uiAutomationFlags = UiAutomation.FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES
            // Every search waits for idle first; an animating screen would hold
            // it 10 s. Settling is ours to do (settle, waits), so don't.
            waitForIdleTimeout = 0
        }
        // The flags above are the ones [ua] was connected with: asked for
        // other ones, Instrumentation.getUiAutomation would reconnect it.
        UiDevice.getInstance(Host.it.instrumentation(ua)).also { events.reclaim() }   // it took the event listener
    }

    fun handle(head: String, cmd: String): ByteArray? {
        if (!head.startsWith("by_")) return null
        val (opts, query) = split(cmd)
        return try {
            val q = Query.parse(query)
            when (head) {
                "by_find" -> nodes(find(q))
                "by_wait" -> waitFor(q, opts)
                "by_tap" -> tap(q, opts)
                "by_swipe" -> swipe(q, opts)
                "by_pinch" -> pinch(q, opts, cmd)
                "by_drag" -> drag(q, opts)
                else -> null
            }
        } catch (e: IllegalArgumentException) {
            err("BAD_ARG", "by:${e.message}")
        } catch (t: Throwable) {
            err("INTERNAL", "$head:${t.javaClass.simpleName}:${t.message}")
        }
    }

    // ── search ──────────────────────────────────────────────────────────

    /**
     * The first node [query] matches (by: syntax, prefix optional; the older
     * `id=… text="…"` form reads the same), in [scope]: all windows, the
     * app's, or the active one. Visible nodes only. Throws
     * IllegalArgumentException for a query it can't read.
     */
    fun first(query: String, scope: String = "all"): AccessibilityNodeInfo? = first(Query.parse(query), scope)

    fun first(q: Query, scope: String = "all"): AccessibilityNodeInfo? {
        val found = when {
            q.window != null -> find(q).firstOrNull()
            scope == "app" -> device.findWindows(By.Window.type(AccessibilityWindowInfo.TYPE_APPLICATION))
                .firstNotNullOfOrNull { it.findObject(q.node) }
            scope == "active" -> device.findWindows(By.Window.active(true)).firstNotNullOfOrNull { it.findObject(q.node) }
            else -> device.findObject(q.node)
        }
        return try { found?.accessibilityNodeInfo } catch (_: Throwable) { null }
    }

    private fun find(q: Query): List<UiObject2> {
        val w = q.window ?: return device.findObjects(q.node)
        return device.findWindows(w).flatMap { it.findObjects(q.node) }
    }

    /** The [nth] (1-based) match, waiting up to [timeoutMs] for it. */
    private fun one(q: Query, opts: Map<String, String>): UiObject2? {
        val nth = (opts["nth"]?.toIntOrNull() ?: 1).coerceAtLeast(1)
        val deadline = System.currentTimeMillis() + (opts["timeout_ms"]?.toLongOrNull() ?: 0)
        while (true) {
            find(q).getOrNull(nth - 1)?.let { return it }
            if (System.currentTimeMillis() >= deadline) return null
            Thread.sleep(100)
        }
    }

    private fun notFound(q: Query) = err("NOT_FOUND", "by:${q.source}")

    /**
     * Waits for [q] to show (or, gone=1, to go), rechecking on every
     * accessibility event, as the daemon's other waits do: no polling, and
     * a change is seen at once. What matches is By's; when to look is ours.
     * Called off the main looper; the wait itself runs there.
     */
    private fun waitFor(q: Query, opts: Map<String, String>): ByteArray {
        val timeout = opts["timeout_ms"]?.toLongOrNull() ?: 10_000
        val gone = opts["gone"] == "1"
        var hit: UiObject2? = null
        val elapsed = kotlinx.coroutines.runBlocking(Loop.Main) {
            events.waits.awaitPredicate(timeout) {
                kotlinx.coroutines.withContext(Loop.Blocking) {
                    val found = find(q)
                    hit = found.firstOrNull()
                    if (gone) found.isEmpty() else hit != null
                }
            }
        }
        if (elapsed < 0) return err("TIMEOUT", (if (gone) "by_gone:" else "by_wait:") + q.source)
        if (gone) return ok("ok gone elapsed=$elapsed")
        val c = hit!!.visibleCenter
        return ok("ok x=${c.x} y=${c.y} elapsed=$elapsed")
    }

    // ── actions ─────────────────────────────────────────────────────────

    private fun tap(q: Query, opts: Map<String, String>): ByteArray {
        val o = one(q, opts) ?: return notFound(q)
        val c = o.visibleCenter
        val newWindowMs = opts["new_window_ms"]?.toLongOrNull() ?: 0
        return when {
            opts["long"] == "1" -> { o.longClick(); ok("ok long x=${c.x} y=${c.y}") }
            newWindowMs > 0 -> {
                val opened = o.clickAndWait(Until.newWindow(), newWindowMs) == true
                ok("ok x=${c.x} y=${c.y} new_window=$opened")
            }
            else -> { o.click(); ok("ok x=${c.x} y=${c.y}") }
        }
    }

    private fun direction(s: String?): Direction = when (s) {
        "up" -> Direction.UP
        "down" -> Direction.DOWN
        "left" -> Direction.LEFT
        "right" -> Direction.RIGHT
        else -> throw IllegalArgumentException("dir must be up, down, left or right")
    }

    /** The finger moves [dir] inside the element. A scrollable one is scrolled
     *  (the content goes the other way) and says when its end is reached. */
    private fun swipe(q: Query, opts: Map<String, String>): ByteArray {
        val o = one(q, opts) ?: return notFound(q)
        val finger = direction(opts["dir"])
        val content = Direction.reverse(finger)
        val pct = opts["pct"]?.toFloatOrNull() ?: 0.8f
        val speed = opts["speed"]?.toIntOrNull()
        return when {
            opts["fling"] == "1" -> ok(if (o.fling(content)) "ok fling" else "ok fling end")
            o.isScrollable -> {
                // The list's own scroll action pages at once and says when it
                // can't go on. Else a swipe, without UiObject2.scroll's wait
                // for the scroll events to stop (~1.5 s): settling is the
                // caller's job.
                val more = scrollAction(o, content) ?: swipeMoved(q, o, finger, pct, speed)
                ok(if (more) "ok scroll" else "ok scroll end")
            }
            else -> { speed?.let { o.swipe(finger, pct, it) } ?: o.swipe(finger, pct); ok("ok swipe") }
        }
    }

    /** Swipes inside [o]; whether its first rows moved (false: the end). */
    private fun swipeMoved(q: Query, o: UiObject2, finger: Direction, pct: Float, speed: Int?): Boolean {
        val before = rows(o)
        if (speed != null) o.swipe(finger, pct, speed) else o.swipe(finger, pct)
        val after = device.findObject(q.node)?.let { rows(it) } ?: return true
        return after != before
    }

    /** Where its first few children are and what they say. */
    private fun rows(o: UiObject2): String = try {
        o.children.take(4).joinToString("|") { c -> c.visibleBounds.toShortString() + (labelOf(c) ?: "") }
    } catch (_: Throwable) { "" }

    /** ACTION_SCROLL_DOWN/UP/LEFT/RIGHT (or, vertically, FORWARD/BACKWARD) when the
     *  node offers it: whether it moved; null when it offers neither. */
    private fun scrollAction(o: UiObject2, content: Direction): Boolean? {
        val n = try { o.accessibilityNodeInfo } catch (_: Throwable) { return null }
        val directional = when (content) {
            Direction.DOWN -> AccessibilityNodeInfo.AccessibilityAction.ACTION_SCROLL_DOWN
            Direction.UP -> AccessibilityNodeInfo.AccessibilityAction.ACTION_SCROLL_UP
            Direction.LEFT -> AccessibilityNodeInfo.AccessibilityAction.ACTION_SCROLL_LEFT
            Direction.RIGHT -> AccessibilityNodeInfo.AccessibilityAction.ACTION_SCROLL_RIGHT
        }
        val offered = n.actionList
        if (offered.any { it.id == directional.id }) return n.performAction(directional.id)
        val vertical = content == Direction.DOWN || content == Direction.UP
        val step = if (content == Direction.DOWN) AccessibilityNodeInfo.ACTION_SCROLL_FORWARD else AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD
        if (vertical && offered.any { it.id == step }) return n.performAction(step)
        return null
    }

    private fun pinch(q: Query, opts: Map<String, String>, cmd: String): ByteArray {
        val o = one(q, opts) ?: return notFound(q)
        val pct = opts["pct"]?.toFloatOrNull() ?: 0.8f
        val inward = cmd.split(' ').getOrNull(1)
        when (inward) {
            "in" -> o.pinchClose(pct)
            "out" -> o.pinchOpen(pct)
            else -> throw IllegalArgumentException("pinch in or out")
        }
        return ok("ok pinch $inward")
    }

    /** Press and hold the element, then move to [to] and let go. */
    private fun drag(q: Query, opts: Map<String, String>): ByteArray {
        val o = one(q, opts) ?: return notFound(q)
        val to = opts["to"]?.split(',')?.mapNotNull { it.trim().toIntOrNull() }?.takeIf { it.size == 2 }
            ?: throw IllegalArgumentException("to=X,Y")
        val speed = opts["speed"]?.toIntOrNull()
        val from = o.visibleCenter
        if (speed != null) o.drag(Point(to[0], to[1]), speed) else o.drag(Point(to[0], to[1]))
        return ok("ok drag ${from.x},${from.y} -> ${to[0]},${to[1]}")
    }

    // ── dialogs and the keyboard (`go accept|dismiss|hide_keyboard`) ───

    /**
     * Answers the dialog on screen, told apart as Appium does: a runtime
     * permission prompt (`…:id/permission_message`; its first allow or deny
     * button, Android's own order: "while using the app" before "only this
     * time") or an alert (`…:id/alertTitle|alert_title|custom`; button1 to
     * accept, button2 to dismiss, the only one when there is one); failing
     * those, a window smaller than the screen ([dialogButtons]).
     */
    fun dialog(accept: Boolean): ByteArray {
        val verb = if (accept) "accept" else "dismiss"
        val button: UiObject2? = if (device.hasObject(By.res(Pattern.compile(".+:id/permission_message")))) {
            val word = if (accept) "allow" else "deny"
            device.findObjects(By.res(Pattern.compile(".+:id/permission_\\w+_button")))
                .firstOrNull { b -> b.resourceName?.substringAfter(":id/")?.split('_')?.contains(word) == true }
        } else if (device.hasObject(By.res(Pattern.compile(".+:id/(alertTitle|alert_title|custom|message)")))) {
            // One button (an OK-only notice) closes it either way.
            val order = if (accept) listOf("button1") else listOf("button2", "button3", "button1")
            order.firstNotNullOfOrNull { id -> device.findObject(By.res(Pattern.compile(".+:id/$id"))) }
        } else {
            val row = dialogButtons() ?: return err("NOT_FOUND", "$verb: no dialog on screen")
            // Material puts confirm on the right, dismiss on the left.
            if (accept) row.last() else row.first()
        }
        val b = button ?: return err("NOT_FOUND", "$verb: the dialog has no button for it")
        val label = labelOf(b) ?: b.resourceName
        b.click()
        return ok("ok $verb \"$label\"")
    }

    /**
     * A dialog with no framework ids (Compose's AlertDialog, custom ones):
     * the active app window, smaller than the screen, with text that isn't a
     * button (a title or message; a popup menu is all items). Its buttons are the
     * labelled clickable nodes in its bottom row, left to right.
     */
    private fun dialogButtons(): List<UiObject2>? {
        val screen = android.graphics.Point()
        @Suppress("DEPRECATION")
        ctx.getSystemService(android.hardware.display.DisplayManager::class.java)
            ?.getDisplay(android.view.Display.DEFAULT_DISPLAY)?.getRealSize(screen)
        val win = device.findWindows(By.Window.type(AccessibilityWindowInfo.TYPE_APPLICATION).active(true))
            .maxByOrNull { it.layer } ?: return null
        val wb = win.visibleBounds
        if (screen.y <= 0 || (wb.width() >= screen.x && wb.height() >= screen.y * 9 / 10)) return null
        // A title or message that isn't a button: not a popup menu, all items.
        if (!win.hasObject(By.clickable(false).text(Pattern.compile(".+", Pattern.DOTALL)))) return null
        val labelled = win.findObjects(By.clickable(true)).filter { labelOf(it) != null }
        if (labelled.isEmpty()) return null
        val bottom = labelled.maxOf { it.visibleBounds.top }
        val rowHeight = labelled.first { it.visibleBounds.top == bottom }.visibleBounds.height()
        return labelled.filter { it.visibleBounds.top >= bottom - rowHeight / 2 }.sortedBy { it.visibleCenter.x }
    }

    /** What a button says: its text or description, or (Compose) a labelled child's. */
    private fun labelOf(o: UiObject2): String? =
        o.text?.takeIf { it.isNotEmpty() } ?: o.contentDescription?.takeIf { it.isNotEmpty() }
            ?: o.findObject(By.text(Pattern.compile(".+", Pattern.DOTALL)))?.text

    /** Back closes the soft keyboard, only while it is up (else it would leave the screen). */
    fun hideKeyboard(back: () -> Unit): ByteArray {
        val up = ua.windows.orEmpty().any { it.type == AccessibilityWindowInfo.TYPE_INPUT_METHOD }
        if (!up) return ok("ok no keyboard")
        back()
        return ok("ok keyboard hidden")
    }

    // ── output ──────────────────────────────────────────────────────────

    private fun nodes(found: List<UiObject2>): ByteArray {
        val sb = StringBuilder(256 + found.size * 160)
        val out = JsonOut(sb)
        out.beginObj().key("nodes").beginArr()
        for (o in found) {
            val n = try { o.accessibilityNodeInfo } catch (_: Throwable) { null } ?: continue
            Traverse.writeFieldsOnly(n, out, o.visibleBounds)
            out.endObj()
        }
        out.endArr().endObj()
        return out.finish().toByteArray()
    }

    private fun ok(s: String) = s.toByteArray()
    private fun err(code: String, msg: String) = "ERR:$code:$msg".toByteArray()

    /** `[opts…] q=QUERY` → (opts, QUERY). */
    private fun split(cmd: String): Pair<Map<String, String>, String> {
        val at = if (cmd.contains(" q=")) cmd.indexOf(" q=") + 1 else -1
        require(at > 0) { "missing q=" }
        val opts = cmd.substring(0, at).trim().split(' ').drop(1)
            .mapNotNull { it.split('=', limit = 2).takeIf { p -> p.size == 2 } }
            .associate { it[0] to it[1] }
        return opts to cmd.substring(at + 2)
    }

    // ── the query language ──────────────────────────────────────────────

    class Query(val node: BySelector, val window: ByWindowSelector?, val source: String) {
        companion object {
            /** Text equal to (or, not [exact], containing) [text]: no quoting to get wrong. */
            fun text(text: String, exact: Boolean): Query {
                val v = Pattern.quote(text)
                return Query(By.text(Pattern.compile(if (exact) v else ".*$v.*", Pattern.DOTALL)), null, "text=$text")
            }

            fun parse(src: String): Query {
                val s = src.trim().removePrefix("by:").trim()
                require(s.isNotEmpty()) { "empty query" }
                var window: ByWindowSelector? = null
                val sel = selector(tokens(s)) { inner -> window = windowSelector(tokens(inner)) }
                return Query(sel ?: throw IllegalArgumentException("window() needs a node term too, e.g. text=…"), window, s)
            }

            /** Splits on spaces outside quotes, parentheses and /regex/. */
            fun tokens(s: String): List<String> {
                val out = ArrayList<String>()
                val cur = StringBuilder()
                var depth = 0
                var quote = false
                var regex = false
                for ((i, ch) in s.withIndex()) {
                    when {
                        quote -> { cur.append(ch); if (ch == '"') quote = false }
                        regex -> { cur.append(ch); if (ch == '/' && s[i - 1] != '\\') regex = false }
                        ch == '"' -> { cur.append(ch); quote = true }
                        ch == '/' && cur.endsWith("=") -> { cur.append(ch); regex = true }
                        ch == '(' -> { depth++; cur.append(ch) }
                        ch == ')' -> { depth--; cur.append(ch) }
                        ch.isWhitespace() && depth == 0 -> { if (cur.isNotEmpty()) out.add(cur.toString()); cur.clear() }
                        else -> cur.append(ch)
                    }
                }
                require(!quote) { "unclosed quote" }
                require(depth == 0) { "unbalanced parentheses" }
                if (cur.isNotEmpty()) out.add(cur.toString())
                return out
            }

            private val CLASS_NAME = Regex("""^[A-Z][\w$]*$|^[a-z]\w*(\.\w+)+$""")
            private val TERM = Regex("""^([a-z]+)(~=|\^=|\$=|=)(.*)$""")
            private val FLAGS = setOf(
                "clickable", "checkable", "checked", "enabled", "focusable", "focused",
                "long-clickable", "scrollable", "selected",
            )

            private fun selector(terms: List<String>, onWindow: (String) -> Unit): BySelector? {
                var sel: BySelector? = null
                fun add(f: (BySelector) -> BySelector, g: () -> BySelector) {
                    sel = sel?.let(f) ?: g()
                }
                for (t in terms) {
                    val call = Regex("""^([a-z]+)\((.*)\)$""").find(t)
                    if (call != null) {
                        val (fn, inner) = call.destructured
                        if (fn == "window") { onWindow(inner); continue }
                        val child = selector(tokens(inner)) { throw IllegalArgumentException("window() only at the top") }
                            ?: throw IllegalArgumentException("$fn() is empty")
                        when (fn) {
                            "has" -> add({ it.hasDescendant(child) }, { By.hasDescendant(child) })
                            "child" -> add({ it.hasChild(child) }, { By.hasChild(child) })
                            "parent" -> add({ it.hasParent(child) }, { By.hasParent(child) })
                            "in" -> add({ it.hasAncestor(child) }, { By.hasAncestor(child) })
                            else -> throw IllegalArgumentException("unknown $fn()")
                        }
                        continue
                    }
                    val neg = t.startsWith('!')
                    val flag = t.removePrefix("!")
                    if (flag in FLAGS) {
                        val v = !neg
                        when (flag) {
                            "clickable" -> add({ it.clickable(v) }, { By.clickable(v) })
                            "checkable" -> add({ it.checkable(v) }, { By.checkable(v) })
                            "checked" -> add({ it.checked(v) }, { By.checked(v) })
                            "enabled" -> add({ it.enabled(v) }, { By.enabled(v) })
                            "focusable" -> add({ it.focusable(v) }, { By.focusable(v) })
                            "focused" -> add({ it.focused(v) }, { By.focused(v) })
                            "long-clickable" -> add({ it.longClickable(v) }, { By.longClickable(v) })
                            "scrollable" -> add({ it.scrollable(v) }, { By.scrollable(v) })
                            "selected" -> add({ it.selected(v) }, { By.selected(v) })
                        }
                        continue
                    }
                    // A bare class name, as in `EditText` or `android.widget.EditText`.
                    if (CLASS_NAME.matches(t)) {
                        val p = pattern("class", "=", t)
                        add({ it.clazz(p) }, { By.clazz(p) })
                        continue
                    }
                    val m = TERM.find(t) ?: throw IllegalArgumentException(
                        "can't read '$t': use key=value (text, desc, hint, res, class, pkg, depth; a value with spaces in \"quotes\"), a flag, has(…), in(…), window(…)"
                    )
                    val (key, op, raw) = m.destructured
                    if (key == "depth") {
                        val d = raw.toIntOrNull() ?: throw IllegalArgumentException("depth=N")
                        add({ it.depth(d) }, { By.depth(d) })
                        continue
                    }
                    val p = pattern(key, op, raw)
                    when (key) {
                        "text" -> add({ it.text(p) }, { By.text(p) })
                        "desc" -> add({ it.desc(p) }, { By.desc(p) })
                        "hint" -> add({ it.hint(p) }, { By.hint(p) })
                        "res", "id" -> add({ it.res(p) }, { By.res(p) })
                        "class", "cls" -> add({ it.clazz(p) }, { By.clazz(p) })
                        "pkg" -> add({ it.pkg(p) }, { By.pkg(p) })
                        else -> throw IllegalArgumentException("unknown key $key")
                    }
                }
                return sel
            }

            /** One pattern for every operator: exact, contains, prefix, suffix, /regex/flags. */
            private fun pattern(key: String, op: String, raw: String): Pattern {
                if (op == "=" && raw.startsWith('/')) {
                    val end = raw.lastIndexOf('/')
                    require(end > 0) { "unclosed /regex/" }
                    val flags = if ('i' in raw.substring(end + 1)) Pattern.CASE_INSENSITIVE or Pattern.UNICODE_CASE else 0
                    return Pattern.compile(raw.substring(1, end), flags or Pattern.DOTALL)
                }
                val v = Pattern.quote(raw.removeSurrounding("\""))
                return Pattern.compile(when (op) {
                    "~=" -> ".*$v.*"
                    "^=" -> "$v.*"
                    "$=" -> ".*$v"
                    // A bare id or class name stands for any package's.
                    else -> when {
                        (key == "res" || key == "id") && ':' !in raw -> "(.*:id/)?$v"
                        (key == "class" || key == "cls") && '.' !in raw -> "(.*\\.)?$v"
                        else -> v
                    }
                }, Pattern.DOTALL)
            }

            private fun windowSelector(terms: List<String>): ByWindowSelector {
                var sel: ByWindowSelector? = null
                fun add(f: (ByWindowSelector) -> ByWindowSelector, g: () -> ByWindowSelector) {
                    sel = sel?.let(f) ?: g()
                }
                for (t in terms) {
                    when (t.removePrefix("!")) {
                        "active" -> { val v = !t.startsWith('!'); add({ it.active(v) }, { By.Window.active(v) }); continue }
                        "focused" -> { val v = !t.startsWith('!'); add({ it.focused(v) }, { By.Window.focused(v) }); continue }
                    }
                    val m = TERM.find(t) ?: throw IllegalArgumentException("window(): can't read '$t'")
                    val (key, op, raw) = m.destructured
                    when (key) {
                        "pkg" -> { val p = pattern(key, op, raw); add({ it.pkg(p) }, { By.Window.pkg(p) }) }
                        "title" -> { val p = pattern(key, op, raw); add({ it.title(p) }, { By.Window.title(p) }) }
                        "display" -> {
                            val d = raw.toIntOrNull() ?: throw IllegalArgumentException("display=N")
                            add({ it.displayId(d) }, { By.Window.displayId(d) })
                        }
                        "type" -> {
                            val ty = when (raw) {
                                "app" -> AccessibilityWindowInfo.TYPE_APPLICATION
                                "ime" -> AccessibilityWindowInfo.TYPE_INPUT_METHOD
                                "system" -> AccessibilityWindowInfo.TYPE_SYSTEM
                                "overlay" -> AccessibilityWindowInfo.TYPE_ACCESSIBILITY_OVERLAY
                                else -> throw IllegalArgumentException("type=app|ime|system|overlay")
                            }
                            add({ it.type(ty) }, { By.Window.type(ty) })
                        }
                        else -> throw IllegalArgumentException("window(): unknown key $key")
                    }
                }
                return sel ?: throw IllegalArgumentException("window() is empty")
            }
        }
    }
}
