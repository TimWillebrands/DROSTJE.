plugins {
    id("com.android.application")
}

android {
    namespace = "app.drostje.phone"
    compileSdk = 36

    defaultConfig {
        applicationId = "app.drostje.phone"
        minSdk = 30
        targetSdk = 36
        versionCode = providers.environmentVariable("ANDROID_VERSION_CODE").orElse("1").get().toInt()
        versionName = providers.environmentVariable("ANDROID_VERSION_NAME").orElse("0.1.0").get()
        ndk {
            abiFilters += listOf("arm64-v8a")
        }
        manifestPlaceholders["nativeLibraryName"] = "drostje_mobile"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    packaging {
        jniLibs {
            useLegacyPackaging = true
        }
    }
}

dependencies {
    implementation("androidx.activity:activity:1.10.1")
    implementation("androidx.camera:camera-camera2:1.4.2")
    implementation("androidx.camera:camera-lifecycle:1.4.2")
    implementation("androidx.camera:camera-view:1.4.2")
}
