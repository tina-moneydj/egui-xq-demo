// 只負責把 cargo-ndk 編好的 libxq_mobile_egui.so 包成 APK（NativeActivity，沒有 Java/Kotlin 程式碼）
buildscript {
    repositories { google(); mavenCentral() }
    dependencies { classpath("com.android.tools.build:gradle:9.1.0") }
}
