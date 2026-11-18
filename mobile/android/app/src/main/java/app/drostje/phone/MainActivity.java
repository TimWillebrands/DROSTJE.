package app.drostje.phone;

import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.provider.Settings;

import dev.gpui.mobile.GpuiInputActivity;

public final class MainActivity extends GpuiInputActivity {
    private static volatile boolean libraryLoaded;

    @Override
    protected void onCreate(Bundle state) {
        loadLibrary();
        super.onCreate(state);
        deliver(getIntent());
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        deliver(intent);
    }

    private void loadLibrary() {
        if (libraryLoaded) {
            return;
        }
        try {
            ActivityInfo info = getPackageManager().getActivityInfo(getComponentName(), PackageManager.GET_META_DATA);
            String library = info.metaData.getString("android.app.lib_name");
            if (library != null) {
                System.loadLibrary(library);
                libraryLoaded = true;
            }
        } catch (PackageManager.NameNotFoundException error) {
            throw new IllegalStateException("MainActivity is declared in its own manifest", error);
        }
    }

    private void deliver(Intent intent) {
        if (intent == null) {
            return;
        }
        Uri data = intent.getData();
        if (data != null && libraryLoaded) {
            nativeLink(data.toString());
        }
    }

    public void startScanner() {
        runOnUiThread(() -> startActivity(new Intent(this, ScanActivity.class)));
    }

    public void leave() {
        runOnUiThread(() -> moveTaskToBack(true));
    }

    public String deviceName() {
        String name = Settings.Global.getString(getContentResolver(), Settings.Global.DEVICE_NAME);
        if (name != null && !name.trim().isEmpty()) {
            return name.trim();
        }
        String model = Build.MODEL == null ? "" : Build.MODEL.trim();
        String maker = Build.MANUFACTURER == null ? "" : Build.MANUFACTURER.trim();
        if (!maker.isEmpty() && !model.toLowerCase().startsWith(maker.toLowerCase())) {
            return maker.substring(0, 1).toUpperCase() + maker.substring(1) + " " + model;
        }
        return model.isEmpty() ? "Android phone" : model;
    }

    static native void nativeLink(String link);
}
