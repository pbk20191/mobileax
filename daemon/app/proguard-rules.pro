# Entry point: app_process invokes Main.main.
-keep public class dev.mobileax.daemon.Main {
    public static void main(java.lang.String[]);
}

# Entry point for am instrument. MobaxInstrumentation lives in the androidTest
# APK, which R8 doesn't see while it shrinks this one: keep exactly what that
# class uses. It gets this APK's mapping applied, so names may change.
-keep,allowobfuscation class androidx.test.runner.MonitoringInstrumentation {
    public <init>();
    public void onCreate(android.os.Bundle);
}
-keep,allowobfuscation class dev.mobileax.daemon.InstrEntryKt {
    public static void startInstrumentation(android.os.Bundle);
}
