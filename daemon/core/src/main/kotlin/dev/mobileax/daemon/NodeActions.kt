package dev.mobileax.daemon

import android.app.UiAutomation
import android.os.Bundle
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityWindowInfo
import java.nio.charset.StandardCharsets
import java.util.ArrayDeque
import java.util.Deque

/**
 * Accessibility-action shortcuts. Lets RPA scripts do things like
 * node_click text="Sign in"
 * node_set_text id=com.foo:id/email text="user@example.com"
 * node_scroll class=androidx.recyclerview.widget.RecyclerView dir=forward
 *
 * Selectors are by: queries ([ByEngine], prefix optional), visible nodes only.
 * 
 * Faster and more reliable than coordinate gestures: no virtual-keyboard
 * round-trip, no race with layout animations, no chance of an overlapping
 * window swallowing the tap.
 */
internal class NodeActions(private val ua: UiAutomation, private val by: ByEngine) {
    // ---------- public action wrappers ----------
    fun click(selectorStr: String): ByteArray? {
        return perform(selectorStr, AccessibilityNodeInfo.ACTION_CLICK, null, "click")
    }

    fun longClick(selectorStr: String): ByteArray? {
        return perform(selectorStr, AccessibilityNodeInfo.ACTION_LONG_CLICK, null, "long_click")
    }

    fun focus(selectorStr: String): ByteArray? {
        return perform(selectorStr, AccessibilityNodeInfo.ACTION_FOCUS, null, "focus")
    }

    fun setText(selectorStr: String?, text: String?): ByteArray? {
        val args = Bundle()
        args.putCharSequence(
            AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, text
        )
        // Empty selector → operate on whichever EditText currently has input
        // focus. Mirrors `paste` / `submit` so callers like mobax tui can update
        // a tapped-to-focus field that has no resource-id without inventing
        // a brittle selector. ACTION_SET_TEXT replaces the field's contents
        // wholesale, so this also covers the "clear before update" case.
        if (selectorStr.isNullOrEmpty()) {
            val target = findInputFocus() ?: return err("no-focused-input")
            val ok: Boolean
            try {
                ok = target.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, args)
            } catch (t: Throwable) {
                return err("set_text-threw:" + t.message)
            }
            return if (ok) ok("ok set_text") else err("set_text-rejected")
        }
        return perform(selectorStr, AccessibilityNodeInfo.ACTION_SET_TEXT, args, "set_text")
    }

    fun scroll(selectorStr: String, dir: String?): ByteArray? {
        val action = if ("backward".equals(dir, ignoreCase = true))
            AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD
        else
            AccessibilityNodeInfo.ACTION_SCROLL_FORWARD
        return perform(selectorStr, action, null, "scroll_$dir")
    }

    /**
     * `scroll_forward`: scroll the biggest list in the app's windows one page
     * on, through ACTION_SCROLL_FORWARD (no fling, no guessed swipe distance).
     * `ok scrolled`; `ERR:scroll-end` when lists exist but none can go on;
     * `ERR:no-scrollable` when there is none.
     */
    fun scrollForward(): ByteArray {
        val roots = ua.windows?.filter { it.type == AccessibilityWindowInfo.TYPE_APPLICATION }?.mapNotNull { it.root }
            ?.takeIf { it.isNotEmpty() } ?: listOfNotNull(ua.rootInActiveWindow)
        var best: AccessibilityNodeInfo? = null
        var bestArea = -1L
        var anyScrollable = false
        val r = android.graphics.Rect()
        for (root in roots) {
            val q: Deque<AccessibilityNodeInfo> = ArrayDeque()
            q.add(root)
            while (q.isNotEmpty()) {
                val n = q.removeFirst()
                if (n.isScrollable && n.isVisibleToUser) {
                    anyScrollable = true
                    if (n.actionList.any { it.id == AccessibilityNodeInfo.ACTION_SCROLL_FORWARD }) {
                        n.getBoundsInScreen(r)
                        val area = r.width().toLong() * r.height()
                        if (area > bestArea) { best = n; bestArea = area }
                    }
                }
                for (i in 0 until n.childCount) n.getChild(i)?.let { q.add(it) }
            }
        }
        val n = best ?: return err(if (anyScrollable) "scroll-end" else "no-scrollable")
        val moved = try { n.performAction(AccessibilityNodeInfo.ACTION_SCROLL_FORWARD) } catch (_: Throwable) { false }
        return if (moved) ok("ok scrolled") else err("scroll-end")
    }

    /**
     * Insert the current system clipboard into the focused EditText (or
     * the one matched by `selectorStr`, if non-null) via
     * `AccessibilityNodeInfo.ACTION_PASTE`. The clipboard itself
     * is set with `mobax clip TEXT` (wire: `clip_set`); this
     * verb is the on-device equivalent of pressing Cmd/Ctrl+V.
     */
    fun pasteAction(selectorStr: String?): ByteArray? {
        val target: AccessibilityNodeInfo?
        if (selectorStr.isNullOrEmpty()) {
            target = findInputFocus()
            if (target == null) return err("no-focused-input")
        } else {
            target = try { by.first(selectorStr) } catch (e: IllegalArgumentException) {
                return err("bad-selector:" + e.message)
            }
            if (target == null) return err("not-found:$selectorStr")
        }
        val ok: Boolean
        try {
            ok = target.performAction(AccessibilityNodeInfo.ACTION_PASTE)
        } catch (t: Throwable) {
            return err("paste-threw:" + t.message)
        }
        return if (ok) ok("ok paste") else err("paste-rejected")
    }

    /**
     * Press the IME action button on the focused EditText (or the one
     * matched by `selectorStr`, if non-null). Routes through
     * `AccessibilityAction.ACTION_IME_ENTER`, which makes the
     * framework fire the field's configured editor action — Search /
     * Go / Send / Done / Next / Previous depending on what the app
     * declared via `android:imeOptions`.
     */
    fun imeAction(selectorStr: String?): ByteArray? {
        val target: AccessibilityNodeInfo?
        if (selectorStr.isNullOrEmpty()) {
            target = findInputFocus()
            if (target == null) return err("no-focused-input")
        } else {
            target = try { by.first(selectorStr) } catch (e: IllegalArgumentException) {
                return err("bad-selector:" + e.message)
            }
            if (target == null) return err("not-found:$selectorStr")
        }
        val actionId: Int
        try {
            actionId = AccessibilityNodeInfo.AccessibilityAction.ACTION_IME_ENTER.id
        } catch (t: Throwable) {
            // ACTION_IME_ENTER is API 30+. Older devices need to send
            // a KEYCODE_ENTER via the input service instead.
            return err("ime-enter-unsupported:" + t.javaClass.getSimpleName())
        }
        val ok: Boolean
        try {
            ok = target.performAction(actionId)
        } catch (t: Throwable) {
            return err("submit-threw:" + t.message)
        }
        return if (ok) ok("ok submit") else err("submit-rejected")
    }

    /** Walk every window's tree looking for the node that holds input focus
     * (an EditText the IME is bound to). Falls back to the active window.  */
    private fun findInputFocus(): AccessibilityNodeInfo? = inputFocus(ua)

    private fun perform(
        selectorStr: String,
        action: Int,
        args: Bundle?,
        opName: String?
    ): ByteArray? {
        val n = try { by.first(selectorStr) } catch (e: IllegalArgumentException) {
            return err("bad-selector:" + e.message)
        } ?: return err("not-found:$selectorStr")
        val ok: Boolean
        try {
            ok = n.performAction(action, args)
        } catch (t: Throwable) {
            return err(opName + "-threw:" + t.message)
        }
        return if (ok) ok("ok $opName") else err("$opName-rejected")
    }

    companion object {
        private fun ok(s: String): ByteArray {
            return s.toByteArray(StandardCharsets.UTF_8)
        }

        private fun err(tail: String?): ByteArray {
            return ("ERR:$tail").toByteArray(StandardCharsets.UTF_8)
        }
    }
}

/**
 * The editable field that has input focus, in any window. Skips the
 * keyboard's own window: while it is up it holds a focused, non-editable
 * view and is listed first.
 */
internal fun inputFocus(ua: UiAutomation): AccessibilityNodeInfo? {
    val editable = { n: AccessibilityNodeInfo? -> n?.takeIf { it.isEditable } }
    for (w in ua.getWindows().orEmpty()) {
        if (w.type == AccessibilityWindowInfo.TYPE_INPUT_METHOD) continue
        editable(w.root?.findFocus(AccessibilityNodeInfo.FOCUS_INPUT))?.let { return it }
    }
    return editable(ua.rootInActiveWindow?.findFocus(AccessibilityNodeInfo.FOCUS_INPUT))
}
