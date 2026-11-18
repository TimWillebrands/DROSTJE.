plugins {
    id("com.android.application")
}

abstract class DesktopLauncherIcon : DefaultTask() {
    @get:InputFile
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val artwork: RegularFileProperty

    @get:OutputDirectory
    abstract val outputDirectory: DirectoryProperty

    @TaskAction
    fun generate() {
        val destination = outputDirectory.file("drawable-nodpi/ic_launcher_desktop.png").get().asFile
        destination.parentFile.mkdirs()
        artwork.get().asFile.copyTo(destination, overwrite = true)
    }
}

androidComponents.onVariants { variant ->
    val icon = tasks.register<DesktopLauncherIcon>("${variant.name}DesktopLauncherIcon") {
        artwork.set(rootProject.layout.projectDirectory.file("../../assets/drostje-app-icon.png"))
    }
    variant.sources.res?.addGeneratedSourceDirectory(icon, DesktopLauncherIcon::outputDirectory)
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
