package com.leanctx.plugin.toolwindow

import com.intellij.openapi.ui.SimpleToolWindowPanel
import com.intellij.ui.components.JBScrollPane
import com.intellij.ui.table.JBTable
import com.intellij.util.ui.JBUI
import com.leanctx.plugin.dto.GainData
import java.awt.BorderLayout
import java.awt.Component
import java.awt.Font
import javax.swing.Box
import javax.swing.BoxLayout
import javax.swing.JLabel
import javax.swing.JPanel
import javax.swing.JProgressBar
import javax.swing.table.DefaultTableModel

/**
 * Renders the gain sections (spec §3, Variante B). Swap the center component per
 * state via [showLoading]/[showError]/[showEmpty]/[showData]. All strings English.
 */
class GainPanel : SimpleToolWindowPanel(true, true) {

    fun showLoading() = setCenter(messagePanel("Loading gain data…"))

    fun showBinaryNotFound() =
        setCenter(messagePanel("lean-ctx binary not found. Run `lean-ctx setup` or check your PATH."))

    fun showEmpty() = setCenter(messagePanel("No data captured yet."))

    fun showError(reason: String, onRetry: () -> Unit) {
        val panel = JPanel(BorderLayout())
        panel.add(messagePanel("Gain command failed:\n$reason"), BorderLayout.CENTER)
        val retry = javax.swing.JButton("Retry").apply { addActionListener { onRetry() } }
        val south = JPanel().apply { add(retry) }
        panel.add(south, BorderLayout.SOUTH)
        setCenter(panel)
    }

    fun showData(data: GainData, ageSeconds: Long) {
        val root = JPanel().apply {
            layout = BoxLayout(this, BoxLayout.Y_AXIS)
            border = JBUI.Borders.empty(8)
        }
        root.add(heroSection(data))
        root.add(Box.createVerticalStrut(8))
        root.add(subScoresSection(data))
        root.add(Box.createVerticalStrut(12))
        root.add(sectionLabel("TASKS BY CATEGORY"))
        root.add(tasksTable(data))
        root.add(Box.createVerticalStrut(12))
        root.add(sectionLabel("HEATMAP · TOP FILES"))
        root.add(heatmapTable(data))
        root.add(Box.createVerticalStrut(8))
        root.add(footer(data, ageSeconds))
        setCenter(JBScrollPane(root))
    }

    private fun setCenter(c: Component) {
        setContent(JPanel(BorderLayout()).apply { add(c, BorderLayout.CENTER) })
        revalidate()
        repaint()
    }

    private fun messagePanel(text: String): JPanel =
        JPanel(BorderLayout()).apply {
            add(JLabel("<html>${text.replace("\n", "<br>")}</html>").apply {
                border = JBUI.Borders.empty(16)
            }, BorderLayout.NORTH)
        }

    private fun sectionLabel(text: String): JComponentLeft =
        JComponentLeft(JLabel(text).apply {
            font = font.deriveFont(Font.BOLD)
            border = JBUI.Borders.empty(4, 0)
        })

    private fun heroSection(d: GainData): Component {
        val s = d.summary
        val score = JLabel("${s.score.total}  GAIN SCORE  ${trendText(s.score.trend)}").apply {
            font = font.deriveFont(Font.BOLD, font.size + 6f)
            foreground = java.awt.Color(0x2E, 0x7D, 0x32) // calm green
        }
        val sub = JLabel(
            "Saved ${tokens(s.tokensSaved)} · Rate ${"%.1f".format(s.gainRatePct)}% · ${usd(s.avoidedUsd)}"
        )
        return JComponentLeft(JPanel().apply {
            layout = BoxLayout(this, BoxLayout.Y_AXIS)
            add(JComponentLeft(score)); add(JComponentLeft(sub))
        })
    }

    private fun subScoresSection(d: GainData): Component {
        val s = d.summary.score
        val panel = JPanel().apply { layout = BoxLayout(this, BoxLayout.Y_AXIS) }
        panel.add(bar("Compression", s.compression))
        panel.add(bar("Cost-Eff.", s.costEfficiency))
        panel.add(bar("Quality", s.quality))
        panel.add(bar("Consistency", s.consistency))
        return JComponentLeft(panel)
    }

    private fun bar(label: String, value: Int): Component {
        val row = JPanel(BorderLayout()).apply { border = JBUI.Borders.empty(1, 0) }
        row.add(JLabel(label).apply { preferredSize = JBUI.size(110, 20) }, BorderLayout.WEST)
        val pb = JProgressBar(0, 100).apply {
            this.value = value
            isStringPainted = true
            string = value.toString()
            foreground = java.awt.Color(0x1E, 0x88, 0xE5) // calm blue, uniform
        }
        row.add(pb, BorderLayout.CENTER)
        return row
    }

    private fun tasksTable(d: GainData): Component {
        val model = object : DefaultTableModel(
            arrayOf("Category", "Cmds", "Saved", "Calls", "$"), 0
        ) { override fun isCellEditable(r: Int, c: Int) = false }
        for (t in d.tasks) {
            model.addRow(arrayOf<Any?>(t.category, t.commands, tokens(t.tokensSaved), t.toolCalls, usd(t.toolSpendUsd)))
        }
        return JBScrollPane(JBTable(model))
    }

    private fun heatmapTable(d: GainData): Component {
        val model = object : DefaultTableModel(
            arrayOf("File", "Access", "Saved", "%"), 0
        ) { override fun isCellEditable(r: Int, c: Int) = false }
        for (f in d.heatmap) {
            model.addRow(arrayOf<Any?>(shortPath(f.path), f.accessCount, tokens(f.tokensSaved), "%.1f".format(f.compressionPct)))
        }
        return JBScrollPane(JBTable(model))
    }

    private fun footer(d: GainData, ageSeconds: Long): Component =
        JComponentLeft(JLabel("Model: ${d.summary.model.modelKey} · updated ${ageSeconds}s ago").apply {
            foreground = java.awt.Color.GRAY
            border = JBUI.Borders.empty(4, 0)
        })

    private fun trendText(trend: String): String = when (trend) {
        "Rising" -> "▲ Rising"
        "Declining" -> "▼ Declining"
        else -> "→ Stable"
    }

    private fun tokens(n: Long): String = when {
        n >= 1_000_000 -> "%.1fM".format(n / 1_000_000.0)
        n >= 1_000 -> "%.1fK".format(n / 1_000.0)
        else -> n.toString()
    }

    private fun usd(amount: Double): String =
        if (amount >= 0.01) "$%.2f".format(amount) else "$%.3f".format(amount)

    private fun shortPath(path: String): String = path.substringAfterLast('/')
}

/** Left-aligns a child inside a BoxLayout (Swing components default to centered). */
private class JComponentLeft(child: Component) : JPanel(BorderLayout()) {
    init {
        add(child, BorderLayout.WEST)
        alignmentX = Component.LEFT_ALIGNMENT
    }
}
