package dev.mobileax.daemon

import android.app.UiAutomation
import android.graphics.Rect
import android.os.Build
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityWindowInfo
import org.w3c.dom.Document
import org.w3c.dom.Element
import org.w3c.dom.NodeList
import java.util.IdentityHashMap
import java.util.regex.Pattern
import javax.xml.XMLConstants
import javax.xml.namespace.NamespaceContext
import javax.xml.namespace.QName
import javax.xml.parsers.DocumentBuilderFactory
import javax.xml.xpath.XPathConstants
import javax.xml.xpath.XPathExpressionException
import javax.xml.xpath.XPathFactory
import javax.xml.xpath.XPathFunction

/**
 * `xpath:` queries over the screen, as Appium's UiAutomator2 server reads
 * them: elements named after the node's class, under a `hierarchy` root,
 * with Appium's attribute names (`text`, `resource-id`, `content-desc`,
 * `clickable`, `bounds` …), so an XPath copied from Appium Inspector works.
 * `bounds` are the visible ones; a node nothing of which shows has
 * `off="below|above|left|right"`.
 *
 * XPath 1.0 (Android's own engine), plus the XPath 2.0 functions people
 * reach for: matches(s, re[, flags]), lower-case, upper-case, ends-with.
 * Appium brought in a whole XPath 2.0 processor for those; four functions
 * don't need one. Class names that aren't XML names (obfuscated ones, which
 * broke Appium's DOM in 2018) are made into one: invalid characters become
 * `_`; `@class` keeps the real name.
 *
 * Wire (options, then `q=` and the query to the end of the line):
 *
 *   xpath_find [scope=app|all] q=Q                         → {"nodes":[…],"ms":N}, or {"value":"…"} for count(), string(), …
 *   xpath_wait [scope=app|all] [timeout_ms=N] [gone=1] q=Q → ok x=… y=… elapsed=N, or ERR:TIMEOUT
 *
 * scope: the app's windows (default: the activity and its dialogs and
 * popups) or every window. Called off the main looper.
 */
internal class XPathEngine(private val ua: UiAutomation, private val events: UiEvents) {

    fun handle(head: String, cmd: String): ByteArray? {
        if (!head.startsWith("xpath_")) return null
        val at = cmd.indexOf(" q=")
        if (at < 0) return err("BAD_ARG", "xpath: missing q=")
        val opts = cmd.substring(0, at).trim().split(' ').drop(1)
            .mapNotNull { it.split('=', limit = 2).takeIf { p -> p.size == 2 } }
            .associate { it[0] to it[1] }
        val q = cmd.substring(at + 3)
        val all = opts["scope"] == "all"
        return try {
            val expr = compile(q)
            when (head) {
                "xpath_find" -> find(expr, all)
                "xpath_wait" -> waitFor(q, expr, all, opts)
                else -> null
            }
        } catch (e: XPathExpressionException) {
            err("BAD_ARG", "xpath:${(e.cause ?: e).message ?: q}")
        } catch (t: Throwable) {
            err("INTERNAL", "$head:${t.javaClass.simpleName}:${t.message}")
        }
    }

    // ── the document ────────────────────────────────────────────────────

    /** The screen as XML, and for each element the node and the clip it shows through. */
    private class Snapshot(val doc: Document) {
        val nodes = IdentityHashMap<Element, Pair<AccessibilityNodeInfo, Rect?>>()
    }

    private class Open(val node: AccessibilityNodeInfo, val el: Element, val count: Int, val clip: Rect?, var next: Int = 0)

    private fun snapshot(all: Boolean): Snapshot {
        val doc = DocumentBuilderFactory.newInstance().newDocumentBuilder().newDocument()
        val snap = Snapshot(doc)
        val top = doc.createElement("hierarchy")
        doc.appendChild(top)
        val windows = ua.windows.orEmpty()
            .filter { all || it.type == AccessibilityWindowInfo.TYPE_APPLICATION }
            .sortedByDescending { it.layer }   // the window on top first, as Appium lists them
        for ((i, w) in windows.withIndex()) {
            val root = try { w.root } catch (_: Throwable) { null } ?: continue
            val clip = Rect().also { w.getBoundsInScreen(it) }
            addTree(snap, top, root, i, clip)
        }
        return snap
    }

    /** [root]'s subtree under [parent], depth-first with an explicit stack (as [Traverse]). */
    private fun addTree(snap: Snapshot, parent: Element, root: AccessibilityNodeInfo, index: Int, clip: Rect) {
        val open = ArrayDeque<Open>()
        var n: AccessibilityNodeInfo? = root
        var into = parent
        var at = index
        var nClip: Rect? = clip
        while (true) {
            if (n != null) {
                val (el, shown) = element(snap.doc, n, at, nClip)
                into.appendChild(el)
                snap.nodes[el] = n to nClip
                val cc = n.childCount
                if (cc > 0) open.addLast(Open(n, el, cc, if (n.isScrollable && shown != null) shown else nClip))
            }
            var advanced = false
            while (open.isNotEmpty()) {
                val o = open.last()
                if (o.next < o.count) {
                    at = o.next
                    n = Traverse.getChildSafe(o.node, o.next++)
                    into = o.el
                    nClip = o.clip
                    advanced = true
                    break
                }
                open.removeLast()
            }
            if (!advanced) return
        }
    }

    /** One node as an element, and the part of it that shows (null: none). */
    private fun element(doc: Document, n: AccessibilityNodeInfo, index: Int, clip: Rect?): Pair<Element, Rect?> {
        val cls = n.className?.toString().orEmpty()
        val el = doc.createElement(xmlName(cls))
        fun attr(k: String, v: CharSequence?) = el.setAttribute(k, clean(v))
        fun flag(k: String, v: Boolean) = el.setAttribute(k, if (v) "true" else "false")
        attr("index", index.toString())
        attr("package", n.packageName)
        attr("class", cls)
        attr("text", n.text)
        val rid = n.viewIdResourceName?.takeIf { it.isNotEmpty() }
            ?: n.extras?.getCharSequence(COMPOSE_TEST_TAG)?.toString()
        attr("resource-id", rid)
        @Suppress("DEPRECATION") // getChecked() is API 36; min API is 28
        val checked = n.isChecked
        flag("checkable", n.isCheckable)
        flag("checked", checked)
        flag("clickable", n.isClickable)
        flag("enabled", n.isEnabled)
        flag("focusable", n.isFocusable)
        flag("focused", n.isFocused)
        flag("long-clickable", n.isLongClickable)
        flag("password", n.isPassword)
        flag("scrollable", n.isScrollable)
        flag("selected", n.isSelected)
        flag("displayed", n.isVisibleToUser)
        attr("content-desc", n.contentDescription)
        n.hintText?.let { attr("hint", it) }
        if (Build.VERSION.SDK_INT >= 30) n.stateDescription?.let { attr("state-description", it) }
        val b = Rect().also { n.getBoundsInScreen(it) }
        val shown = Rect(b).takeIf { clip == null || it.intersect(clip) }
        val r = shown ?: b
        attr("bounds", "[${r.left},${r.top}][${r.right},${r.bottom}]")
        if (shown == null && clip != null) attr("off", offSide(b, clip))
        return el to shown
    }

    // ── evaluation ──────────────────────────────────────────────────────

    private fun compile(q: String) = XPathFactory.newInstance().newXPath().apply {
        namespaceContext = Ns
        setXPathFunctionResolver { name, _ -> if (name.namespaceURI == NS) FUNCTIONS[name.localPart] else null }
    }.compile(withPrefix(q))

    private fun find(expr: javax.xml.xpath.XPathExpression, all: Boolean): ByteArray {
        val t0 = System.currentTimeMillis()
        val snap = snapshot(all)
        val built = System.currentTimeMillis() - t0
        val hits = try {
            expr.evaluate(snap.doc, XPathConstants.NODESET) as NodeList
        } catch (e: XPathExpressionException) {
            // count(), string(), a comparison: not nodes, a value.
            val v = expr.evaluate(snap.doc)
            return JsonOut(StringBuilder()).beginObj().str("value", v).endObj().finish().toByteArray()
        }
        val sb = StringBuilder(256 + hits.length * 160)
        val out = JsonOut(sb)
        out.beginObj().key("nodes").beginArr()
        for (i in 0 until hits.length) {
            val (n, clip) = (hits.item(i) as? Element)?.let { snap.nodes[it] } ?: continue
            Traverse.writeFieldsClipped(n, out, clip)
            out.endObj()
        }
        out.endArr()
        out.num("ms", System.currentTimeMillis() - t0).num("build_ms", built)
        out.endObj()
        return out.finish().toByteArray()
    }

    /** As by_wait: the query is rechecked on every accessibility event, not polled. */
    private fun waitFor(q: String, expr: javax.xml.xpath.XPathExpression, all: Boolean, opts: Map<String, String>): ByteArray {
        val timeout = opts["timeout_ms"]?.toLongOrNull() ?: 10_000
        val gone = opts["gone"] == "1"
        var hit: Rect? = null
        fun check(): Boolean {
            val list = expr.evaluate(snapshot(all).doc, XPathConstants.NODESET) as NodeList
            // Not there yet: one that doesn't show.
            hit = (0 until list.length).firstNotNullOfOrNull { i ->
                val el = list.item(i) as? Element ?: return@firstNotNullOfOrNull null
                if (el.hasAttribute("off")) null else parseBounds(el.getAttribute("bounds"))
            }
            return if (gone) hit == null else hit != null
        }
        // The first check here, not in the wait, which swallows errors: a
        // bad regex in matches() is BAD_ARG at once, not a TIMEOUT later.
        val elapsed = if (check()) 0L else kotlinx.coroutines.runBlocking(Loop.Main) {
            events.waits.awaitPredicate(timeout) {
                kotlinx.coroutines.withContext(Loop.Blocking) { check() }
            }
        }
        if (elapsed < 0) return err("TIMEOUT", (if (gone) "xpath_gone:" else "xpath_wait:") + q)
        if (gone) return "ok gone elapsed=$elapsed".toByteArray()
        val r = hit!!
        return "ok x=${r.centerX()} y=${r.centerY()} elapsed=$elapsed".toByteArray()
    }

    private fun err(code: String, msg: String) = "ERR:$code:$msg".toByteArray()

    companion object {
        private const val COMPOSE_TEST_TAG = "androidx.compose.ui.semantics.testTag"
        private const val NS = "urn:mobileax:xpath2"

        private object Ns : NamespaceContext {
            override fun getNamespaceURI(prefix: String?) = if (prefix == "mx") NS else XMLConstants.NULL_NS_URI
            override fun getPrefix(uri: String?) = if (uri == NS) "mx" else null
            override fun getPrefixes(uri: String?): Iterator<String> = listOfNotNull(getPrefix(uri)).iterator()
        }

        /** XPath 2.0's matches / lower-case / upper-case / ends-with. */
        private val FUNCTIONS: Map<String, XPathFunction> = mapOf(
            "matches" to XPathFunction { a ->
                val flags = a.getOrNull(2)?.let(::str).orEmpty()
                var f = 0
                if ('i' in flags) f = f or Pattern.CASE_INSENSITIVE or Pattern.UNICODE_CASE
                if ('s' in flags) f = f or Pattern.DOTALL
                if ('m' in flags) f = f or Pattern.MULTILINE
                if ('x' in flags) f = f or Pattern.COMMENTS
                Pattern.compile(str(a.getOrNull(1)), f).matcher(str(a.getOrNull(0))).find()
            },
            "lower-case" to XPathFunction { a -> str(a.getOrNull(0)).lowercase() },
            "upper-case" to XPathFunction { a -> str(a.getOrNull(0)).uppercase() },
            "ends-with" to XPathFunction { a -> str(a.getOrNull(0)).endsWith(str(a.getOrNull(1))) },
        )

        /** An argument's string value: a node-set's first node, as XPath's string(). */
        private fun str(v: Any?): String = when (v) {
            null -> ""
            is NodeList -> if (v.length == 0) "" else v.item(0).textContent.orEmpty()
            is org.w3c.dom.Node -> v.textContent.orEmpty()
            is Double -> if (v == Math.floor(v) && !v.isInfinite()) v.toLong().toString() else v.toString()
            else -> v.toString()
        }

        /**
         * `matches(` → `mx:matches(` (and the other three) outside string
         * literals: JAXP looks up only prefixed functions in a resolver.
         */
        internal fun withPrefix(q: String): String {
            val sb = StringBuilder(q.length + 16)
            var i = 0
            while (i < q.length) {
                val c = q[i]
                if (c == '\'' || c == '"') {
                    val end = q.indexOf(c, i + 1).let { if (it < 0) q.length - 1 else it }
                    sb.append(q, i, end + 1)
                    i = end + 1
                    continue
                }
                val prev = if (i > 0) q[i - 1] else ' '
                val name = FUNCTIONS.keys.firstOrNull { q.startsWith(it, i) }
                if (name != null && !(prev.isLetterOrDigit() || prev in ":-_.@$")) {
                    var j = i + name.length
                    while (j < q.length && q[j].isWhitespace()) j++
                    if (j < q.length && q[j] == '(') {
                        sb.append("mx:").append(name)
                        i += name.length
                        continue
                    }
                }
                sb.append(c)
                i++
            }
            return sb.toString()
        }

        /**
         * [cls] as an XML element name: `android.widget.Button` stays; any
         * other character becomes `_`. ASCII only: which other letters a DOM
         * takes as a name differs between XML editions and implementations.
         */
        internal fun xmlName(cls: String): String {
            if (cls.isEmpty()) return "node"
            val sb = StringBuilder(cls.length)
            for ((i, c) in cls.withIndex()) {
                val letter = c in 'a'..'z' || c in 'A'..'Z' || c == '_'
                val ok = letter || (i > 0 && (c in '0'..'9' || c == '.' || c == '-'))
                sb.append(if (ok) c else '_')
            }
            return sb.toString()
        }

        /** Characters XML 1.0 can't hold (controls other than tab and newlines) dropped. */
        private fun clean(v: CharSequence?): String {
            if (v == null) return ""
            val sb = StringBuilder(v.length)
            for (c in v) if (c >= ' ' || c == '\t' || c == '\n' || c == '\r') sb.append(c)
            return sb.toString()
        }

        private fun offSide(b: Rect, c: Rect) = when {
            b.top >= c.bottom -> "below"
            b.bottom <= c.top -> "above"
            b.left >= c.right -> "right"
            else -> "left"
        }

        private fun parseBounds(s: String): Rect? {
            val m = Regex("""\[(-?\d+),(-?\d+)]\[(-?\d+),(-?\d+)]""").matchEntire(s) ?: return null
            val (l, t, r, b) = m.destructured
            return Rect(l.toInt(), t.toInt(), r.toInt(), b.toInt())
        }
    }
}
