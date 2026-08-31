package com.skeidno.networkmanager.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

private val LightColors = lightColorScheme(
    primary = Color(0xFF087F63),
    onPrimary = Color.White,
    primaryContainer = Color(0xFFDDF4EC),
    onPrimaryContainer = Color(0xFF075B48),
    secondary = Color(0xFF0B73E0),
    onSecondary = Color.White,
    secondaryContainer = Color(0xFFE1EFFF),
    onSecondaryContainer = Color(0xFF0A4F9D),
    tertiary = Color(0xFFE76F51),
    background = Color(0xFFF3F5F7),
    surface = Color.White,
    surfaceVariant = Color(0xFFF0F3F6),
    outline = Color(0xFFD7DDE3),
    onSurface = Color(0xFF17202A),
    onSurfaceVariant = Color(0xFF647180),
    error = Color(0xFFD43D45),
)

private val DarkColors = darkColorScheme(
    primary = Color(0xFF65D6B4),
    secondary = Color(0xFF78B7FF),
    tertiary = Color(0xFFFF9A7D),
    background = Color(0xFF111518),
    surface = Color(0xFF181D21),
    surfaceVariant = Color(0xFF22282E),
    outline = Color(0xFF3B444D),
)

@Composable
fun NetworkManagerTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = if (isSystemInDarkTheme()) DarkColors else LightColors,
        content = content,
    )
}
