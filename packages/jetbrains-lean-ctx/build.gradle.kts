plugins {
    id("java")
    id("org.jetbrains.kotlin.jvm") version "1.9.25"
    id("org.jetbrains.intellij.platform") version "2.14.0"
}

group = "com.leanctx"
version = "1.0.0"

repositories {
    mavenCentral()
    intellijPlatform {
        defaultRepositories()
    }
}

dependencies {
    intellijPlatform {
        create("IC", "2024.1")
    }
}

intellijPlatform {
    pluginConfiguration {
        name = "lean-ctx"
        version = project.version.toString()
        ideaVersion {
            sinceBuild = "241"
            untilBuild = "261.*"
        }
        vendor {
            name = "lean-ctx"
            url = "https://github.com/yvgude/lean-ctx"
        }
    }
}

tasks {
    withType<org.jetbrains.kotlin.gradle.tasks.KotlinCompile> {
        kotlinOptions.jvmTarget = "17"
    }
}
