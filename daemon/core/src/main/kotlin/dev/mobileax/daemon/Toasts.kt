package dev.mobileax.daemon

import android.view.accessibility.AccessibilityEvent
import java.nio.charset.StandardCharsets

/**
 * Toasts as they show. A toast leaves no node to read: it is a
 * TYPE_NOTIFICATION_STATE_CHANGED event from android.widget.Toast carrying
 * its text. Each is kept with the event sequence it arrived at, so
 * `toasts since=SEQ` answers "what toasted after this action". Main thread
 * only.
 */
class Toasts(private val events: UiEvents) {

    private val recent = ArrayDeque<Pair<Long, String>>()

    init {
        Loop.post { events.subscribe { ev -> onEvent(ev) } }
    }

    private fun onEvent(ev: AccessibilityEvent) {
        if (ev.eventType != AccessibilityEvent.TYPE_NOTIFICATION_STATE_CHANGED) return
        if (ev.className?.toString()?.contains("Toast") != true) return
        // The message first; later entries (the app's label) are not the toast's words.
        val text = ev.text.firstOrNull()?.toString()?.trim().orEmpty()
        if (text.isEmpty()) return
        // UiEvents counts the event before handing it out: this is its number.
        recent.addLast(events.waits.seq to text)
        while (recent.size > KEEP) recent.removeFirst()
    }

    /** `toasts since=SEQ`: the toasts after event SEQ, one per line, oldest first; empty for none. */
    fun since(cmd: String): ByteArray {
        val seq = Server.longArg(cmd, "since", -1)
        if (seq < 0) return Server.errBytes("BAD_ARG:toasts-needs-since")
        return recent.filter { it.first > seq }.joinToString("\n") { it.second }.toByteArray(StandardCharsets.UTF_8)
    }

    private companion object {
        const val KEEP = 16
    }
}
