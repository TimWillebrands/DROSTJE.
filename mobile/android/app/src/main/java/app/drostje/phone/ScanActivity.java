package app.drostje.phone;

import android.Manifest;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.Path;
import android.graphics.RectF;
import android.graphics.drawable.GradientDrawable;
import android.net.Uri;
import android.os.Bundle;
import android.provider.Settings;
import android.util.Size;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.HapticFeedbackConstants;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.TextView;

import androidx.activity.ComponentActivity;
import androidx.activity.result.ActivityResultLauncher;
import androidx.activity.result.contract.ActivityResultContracts;
import androidx.camera.core.CameraSelector;
import androidx.camera.core.ImageAnalysis;
import androidx.camera.core.ImageProxy;
import androidx.camera.core.Preview;
import androidx.camera.core.resolutionselector.ResolutionSelector;
import androidx.camera.core.resolutionselector.ResolutionStrategy;
import androidx.camera.lifecycle.ProcessCameraProvider;
import androidx.camera.view.PreviewView;

import com.google.common.util.concurrent.ListenableFuture;

import java.nio.ByteBuffer;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicBoolean;

public final class ScanActivity extends ComponentActivity {
    private static final String PAIRING_PREFIX = "drostje://pair";
    private static final long WRONG_CODE_HINT_MS = 2500;

    private final AtomicBoolean finished = new AtomicBoolean(false);
    private final ExecutorService analysis = Executors.newSingleThreadExecutor();
    private PreviewView preview;
    private Viewfinder viewfinder;
    private TextView hint;
    private LinearLayout permissionPanel;
    private TextView permissionText;
    private Button permissionButton;
    private long wrongCodeShownAt;
    private boolean askedOnce;

    private final ActivityResultLauncher<String> permission = registerForActivityResult(
            new ActivityResultContracts.RequestPermission(), this::onPermission);

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        getWindow().setDecorFitsSystemWindows(false);
        setContentView(layout());
        if (hasCamera()) {
            startCamera();
        } else {
            askedOnce = true;
            permission.launch(Manifest.permission.CAMERA);
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        if (askedOnce && hasCamera() && permissionPanel.getVisibility() == View.VISIBLE) {
            startCamera();
        }
    }

    @Override
    protected void onDestroy() {
        analysis.shutdown();
        super.onDestroy();
    }

    private boolean hasCamera() {
        return checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED;
    }

    private int dp(float value) {
        return Math.round(TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, value, getResources().getDisplayMetrics()));
    }

    private View layout() {
        FrameLayout root = new FrameLayout(this);
        root.setBackgroundColor(Color.BLACK);

        preview = new PreviewView(this);
        preview.setScaleType(PreviewView.ScaleType.FILL_CENTER);
        root.addView(preview, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));

        viewfinder = new Viewfinder(this);
        root.addView(viewfinder, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));

        LinearLayout top = new LinearLayout(this);
        top.setOrientation(LinearLayout.HORIZONTAL);
        top.setGravity(Gravity.CENTER_VERTICAL);
        Button close = new Button(this, null, android.R.attr.borderlessButtonStyle);
        close.setText("✕");
        close.setTextColor(Color.WHITE);
        close.setTextSize(TypedValue.COMPLEX_UNIT_SP, 22);
        close.setContentDescription(getString(R.string.scan_close));
        close.setOnClickListener(view -> finish());
        top.addView(close, new LinearLayout.LayoutParams(dp(56), dp(56)));
        TextView title = new TextView(this);
        title.setText(R.string.scan_title);
        title.setTextColor(Color.WHITE);
        title.setTextSize(TypedValue.COMPLEX_UNIT_SP, 20);
        top.addView(title, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f));
        FrameLayout.LayoutParams topParams = new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.TOP);
        root.addView(top, topParams);

        hint = new TextView(this);
        hint.setText(R.string.scan_hint);
        hint.setTextColor(Color.WHITE);
        hint.setTextSize(TypedValue.COMPLEX_UNIT_SP, 16);
        hint.setGravity(Gravity.CENTER);
        hint.setPadding(dp(24), dp(12), dp(24), dp(12));
        GradientDrawable pill = new GradientDrawable();
        pill.setColor(Color.argb(160, 0, 0, 0));
        pill.setCornerRadius(dp(6));
        hint.setBackground(pill);
        FrameLayout.LayoutParams hintParams = new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM);
        hintParams.setMargins(dp(24), 0, dp(24), dp(32));
        root.addView(hint, hintParams);

        permissionPanel = new LinearLayout(this);
        permissionPanel.setOrientation(LinearLayout.VERTICAL);
        permissionPanel.setGravity(Gravity.CENTER);
        permissionPanel.setPadding(dp(32), dp(32), dp(32), dp(32));
        permissionPanel.setBackgroundColor(Color.BLACK);
        permissionPanel.setVisibility(View.GONE);
        permissionText = new TextView(this);
        permissionText.setTextColor(Color.WHITE);
        permissionText.setTextSize(TypedValue.COMPLEX_UNIT_SP, 18);
        permissionText.setGravity(Gravity.CENTER);
        permissionPanel.addView(permissionText, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        permissionButton = new Button(this);
        LinearLayout.LayoutParams buttonParams = new LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, dp(52));
        buttonParams.topMargin = dp(24);
        permissionPanel.addView(permissionButton, buttonParams);
        root.addView(permissionPanel, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        permissionPanel.bringToFront();
        top.bringToFront();

        root.setOnApplyWindowInsetsListener((view, insets) -> {
            android.graphics.Insets bars = insets.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
            top.setPadding(bars.left + dp(4), bars.top, bars.right + dp(16), 0);
            FrameLayout.LayoutParams params = (FrameLayout.LayoutParams) hint.getLayoutParams();
            params.bottomMargin = bars.bottom + dp(32);
            hint.setLayoutParams(params);
            return insets;
        });
        return root;
    }

    private void onPermission(boolean granted) {
        if (granted) {
            startCamera();
            return;
        }
        boolean canAsk = shouldShowRequestPermissionRationale(Manifest.permission.CAMERA);
        permissionText.setText(R.string.scan_permission);
        if (canAsk) {
            permissionButton.setText(R.string.scan_allow);
            permissionButton.setOnClickListener(view -> permission.launch(Manifest.permission.CAMERA));
        } else {
            permissionButton.setText(R.string.scan_settings);
            permissionButton.setOnClickListener(view -> startActivity(new Intent(
                    Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                    Uri.fromParts("package", getPackageName(), null))));
        }
        permissionPanel.setVisibility(View.VISIBLE);
    }

    private void startCamera() {
        permissionPanel.setVisibility(View.GONE);
        ListenableFuture<ProcessCameraProvider> future = ProcessCameraProvider.getInstance(this);
        future.addListener(() -> {
            try {
                bind(future.get());
            } catch (Exception error) {
                showNoCamera();
            }
        }, getMainExecutor());
    }

    private void showNoCamera() {
        permissionText.setText(R.string.scan_no_camera);
        permissionButton.setText(R.string.scan_close);
        permissionButton.setOnClickListener(view -> finish());
        permissionPanel.setVisibility(View.VISIBLE);
    }

    private void bind(ProcessCameraProvider provider) {
        CameraSelector selector = CameraSelector.DEFAULT_BACK_CAMERA;
        try {
            if (!provider.hasCamera(selector)) {
                selector = CameraSelector.DEFAULT_FRONT_CAMERA;
            }
        } catch (Exception error) {
            showNoCamera();
            return;
        }
        Preview shown = new Preview.Builder().build();
        shown.setSurfaceProvider(preview.getSurfaceProvider());
        ImageAnalysis frames = new ImageAnalysis.Builder()
                .setResolutionSelector(new ResolutionSelector.Builder()
                        .setResolutionStrategy(new ResolutionStrategy(new Size(1280, 720),
                                ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER))
                        .build())
                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                .build();
        frames.setAnalyzer(analysis, this::analyze);
        provider.unbindAll();
        try {
            provider.bindToLifecycle(this, selector, shown, frames);
        } catch (Exception error) {
            showNoCamera();
        }
    }

    private void analyze(ImageProxy image) {
        try {
            if (finished.get()) {
                return;
            }
            ImageProxy.PlaneProxy luma = image.getPlanes()[0];
            ByteBuffer buffer = luma.getBuffer();
            String text = nativeDecode(buffer, image.getWidth(), image.getHeight(), luma.getRowStride());
            if (text == null) {
                return;
            }
            if (text.startsWith(PAIRING_PREFIX)) {
                if (finished.compareAndSet(false, true)) {
                    runOnUiThread(() -> {
                        viewfinder.performHapticFeedback(HapticFeedbackConstants.CONFIRM);
                        MainActivity.nativeLink(text);
                        finish();
                    });
                }
            } else {
                runOnUiThread(this::showWrongCode);
            }
        } finally {
            image.close();
        }
    }

    private void showWrongCode() {
        long now = System.currentTimeMillis();
        if (now - wrongCodeShownAt < WRONG_CODE_HINT_MS) {
            return;
        }
        wrongCodeShownAt = now;
        hint.setText(R.string.scan_wrong_code);
        hint.postDelayed(() -> hint.setText(R.string.scan_hint), WRONG_CODE_HINT_MS);
    }

    private static native String nativeDecode(ByteBuffer luma, int width, int height, int rowStride);

    private static final class Viewfinder extends View {
        private final Paint shade = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final Paint corner = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final Path hole = new Path();
        private final RectF frame = new RectF();

        Viewfinder(Context context) {
            super(context);
            shade.setColor(Color.argb(140, 0, 0, 0));
            corner.setColor(Color.WHITE);
            corner.setStyle(Paint.Style.STROKE);
            corner.setStrokeCap(Paint.Cap.ROUND);
            corner.setStrokeWidth(TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, 4, context.getResources().getDisplayMetrics()));
        }

        @Override
        protected void onDraw(Canvas canvas) {
            float width = getWidth();
            float height = getHeight();
            float side = Math.min(width, height) * 0.7f;
            float left = (width - side) / 2f;
            float top = (height - side) / 2f;
            frame.set(left, top, left + side, top + side);
            float radius = side * 0.08f;
            hole.reset();
            hole.setFillType(Path.FillType.EVEN_ODD);
            hole.addRect(0, 0, width, height, Path.Direction.CW);
            hole.addRoundRect(frame, radius, radius, Path.Direction.CW);
            canvas.drawPath(hole, shade);
            float arm = side * 0.16f;
            drawCorner(canvas, frame.left, frame.top, arm, arm);
            drawCorner(canvas, frame.right, frame.top, -arm, arm);
            drawCorner(canvas, frame.left, frame.bottom, arm, -arm);
            drawCorner(canvas, frame.right, frame.bottom, -arm, -arm);
        }

        private void drawCorner(Canvas canvas, float x, float y, float dx, float dy) {
            canvas.drawLine(x, y, x + dx, y, corner);
            canvas.drawLine(x, y, x, y + dy, corner);
        }
    }
}
