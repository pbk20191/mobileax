package dev.mobileax.bench.toast;

import android.app.Activity;
import android.os.Bundle;
import android.view.Gravity;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.Toast;

/** One button; its only result is a toast, which leaves nothing on screen to read.
 *  Java, so the APK carries no Kotlin runtime (a few KB to install). */
public class MainActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Button button = new Button(this);
        button.setText("Show toast");
        button.setOnClickListener(v -> Toast.makeText(this, "Saved", Toast.LENGTH_SHORT).show());
        FrameLayout root = new FrameLayout(this);
        root.addView(button, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.WRAP_CONTENT, FrameLayout.LayoutParams.WRAP_CONTENT, Gravity.CENTER));
        setContentView(root);
    }
}
