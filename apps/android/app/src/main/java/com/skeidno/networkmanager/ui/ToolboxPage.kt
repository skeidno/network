package com.skeidno.networkmanager.ui

import android.content.Intent
import android.widget.Toast
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.AltRoute
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.FactCheck
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Code
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Public
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.SupervisorAccount
import androidx.compose.material.icons.filled.TravelExplore
import androidx.compose.material.icons.filled.VerifiedUser
import androidx.compose.material.icons.filled.WifiTethering
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.graphics.drawable.toBitmap
import com.skeidno.networkmanager.data.AppState
import com.skeidno.networkmanager.data.DEFAULT_PROFILE_ID
import com.skeidno.networkmanager.data.ExitCheckStatus
import com.skeidno.networkmanager.data.ExitInfo
import com.skeidno.networkmanager.data.InstalledApp
import com.skeidno.networkmanager.data.LocalDnsEntry
import com.skeidno.networkmanager.data.MAX_PROFILES
import com.skeidno.networkmanager.data.NetworkTools
import com.skeidno.networkmanager.data.PROFILE_NAME_MAX_LENGTH
import com.skeidno.networkmanager.data.ProfileSummary
import com.skeidno.networkmanager.data.normalizeLocalDnsDomain
import com.skeidno.networkmanager.data.profileNameError
import com.skeidno.networkmanager.data.sanitizeProfileName
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

// ---------------------------------------------------------------------------
// 工具目录：这一页只负责「找得到」，每个工具自己一页
// ---------------------------------------------------------------------------

/** 工具箱分组，枚举顺序就是目录页从上到下的顺序。 */
private enum class ToolGroup(val label: String) {
    Data("数据与档案"),
    Diagnostics("连接诊断"),
    Certificates("证书与 HTTPS"),
    Utilities("文本与处理"),
}

/**
 * 工具箱里的一个工具。
 *
 * `summary` 是宫格上那一行短说明，要短到两行以内；详情页里卡片上那句详细说明
 * 仍写在各自的卡里。`keywords` 是搜索用的别名（拼音不好用，就补英文与常见叫法），
 * 让用户敲 "dns" / "tcp" / "base64" 也能命中中文标题的工具。
 */
private enum class ToolId(
    val title: String,
    val summary: String,
    val group: ToolGroup,
    val icon: ImageVector,
    val keywords: String,
) {
    Profiles(
        title = "用户档案（数据隔离）",
        summary = "多套彼此隔离的数据，等于浏览器的多个 userdir",
        group = ToolGroup.Data,
        icon = Icons.Default.SupervisorAccount,
        keywords = "profile userdir 隔离 分身 账号 切换 用户",
    ),
    Exit(
        title = "出口检测",
        summary = "核对直连与代理的出口 IP，确认节点真的生效",
        group = ToolGroup.Diagnostics,
        icon = Icons.Default.Public,
        keywords = "exit ip 出口 检测 出口ip",
    ),
    RoutePreview(
        title = "规则命中预判",
        summary = "输入域名，看它会命中哪条规则、从哪出去",
        group = ToolGroup.Diagnostics,
        icon = Icons.AutoMirrored.Filled.AltRoute,
        keywords = "route rule 规则 命中 分流 预判 路由",
    ),
    Dns(
        title = "DNS 查询",
        summary = "走 DoH 查 A / AAAA / TXT，可指定从代理出去",
        group = ToolGroup.Diagnostics,
        icon = Icons.Default.TravelExplore,
        keywords = "dns doh 解析 查询 域名 record",
    ),
    Tcp(
        title = "连通性测试",
        summary = "对 host:port 直连握手，区分端口不通与代理没生效",
        group = ToolGroup.Diagnostics,
        icon = Icons.Default.WifiTethering,
        keywords = "tcp 连通 ping 端口 port 握手 测速",
    ),
    LocalCert(
        title = "本地证书",
        summary = "按当前映射签发局域网可用的 CA 与叶证书",
        group = ToolGroup.Certificates,
        icon = Icons.Default.VerifiedUser,
        keywords = "ca 证书 签发 生成 本地 https 叶证书",
    ),
    Certificate(
        title = "证书检测",
        summary = "对域名做真实 TLS 握手，看系统信不信任这张证书",
        group = ToolGroup.Certificates,
        icon = Icons.AutoMirrored.Filled.FactCheck,
        keywords = "证书 检测 tls 握手 信任 报红",
    ),
    TextTool(
        title = "编解码与链接解析",
        summary = "Base64 / URL 编解码、生成 UUID、拆分享链接字段",
        group = ToolGroup.Utilities,
        icon = Icons.Default.Code,
        keywords = "base64 url uuid 编码 解码 链接 解析 转换",
    ),
    ;

    /** 关键字命中：标题、短说明、分组名、别名任一包含即算命中。 */
    fun matches(query: String): Boolean {
        val needle = query.lowercase()
        return title.lowercase().contains(needle) ||
            summary.lowercase().contains(needle) ||
            group.label.lowercase().contains(needle) ||
            keywords.lowercase().contains(needle)
    }
}

/**
 * 工具箱：先给一页「工具目录」（搜索 + 分组宫格），点进去才打开具体工具。
 *
 * 原来所有卡片平铺在一页里，工具一多就得靠滚——想找 DNS 查询得划过证书、编解码
 * 一堆卡片。现在目录页只负责「快速找到」，每个工具独占一页，系统返回键直接回目录。
 *
 * 绝大多数工具是**只读的本地 / 网络探测**——不改配置、不碰内核、不动订阅，随便点
 * 都不会把正在跑的代理弄坏。唯一的例外是「用户档案」：它管的是数据根目录切换
 * （配置管理，不是诊断），所以单独一组、单独一页，别和只读探测混在一起。
 */
@Composable
internal fun ToolboxPage(
    state: AppState,
    profiles: List<ProfileSummary>,
    installedApps: List<InstalledApp>,
    onTestExit: () -> Unit,
    onGenerateLocalCert: () -> Unit,
    onInstallLocalCa: () -> Intent?,
    onInstallLocalCaWithRoot: () -> Unit,
    onExportLocalCa: () -> Unit,
    onRefreshProfiles: () -> Unit,
    onCreateProfile: (String, List<String>) -> Unit,
    onDuplicateProfile: () -> Unit,
    onSwitchProfile: (String) -> Unit,
    onRenameProfile: (String, String) -> Unit,
    onDeleteProfile: (String) -> Unit,
    onSetProfilePackages: (List<String>) -> Unit,
) {
    // 导航状态存名字（字符串），rememberSaveable 能直接存，转屏后停在原处。
    var openTool by rememberSaveable { mutableStateOf<String?>(null) }
    var openProfileId by rememberSaveable { mutableStateOf<String?>(null) }
    val tool = openTool?.let { name -> ToolId.entries.firstOrNull { it.name == name } }
    // 档案被删掉时就找不到它了，详情页自然退回列表，不会停在一个不存在的档案上。
    val openProfile = openProfileId?.let { id -> profiles.firstOrNull { it.id == id } }

    if (tool == null) {
        ToolCatalog(onOpen = { openTool = it.name })
        return
    }

    // 「用户档案」里的第二级：点某个档案进来，才能切换 / 改名 / 调它的 App 名单。
    if (tool == ToolId.Profiles && openProfile != null) {
        BackHandler { openProfileId = null }
        ToolDetail(title = openProfile.name, onBack = { openProfileId = null }) {
            ProfileDetailCard(
                profile = openProfile,
                otherNames = profiles.filterNot { it.id == openProfile.id }.map(ProfileSummary::name),
                installedApps = installedApps,
                onSwitch = { onSwitchProfile(openProfile.id) },
                onRename = { onRenameProfile(openProfile.id, it) },
                onDelete = {
                    onDeleteProfile(openProfile.id)
                    openProfileId = null
                },
                onSavePackages = onSetProfilePackages,
            )
        }
        return
    }

    // 系统返回键在工具页里要回目录，而不是直接退出应用。
    BackHandler { openTool = null }
    ToolDetail(title = tool.title, onBack = { openTool = null }) {
        when (tool) {
            ToolId.Profiles -> ProfilesCard(
                profiles = profiles,
                installedApps = installedApps,
                onOpenProfile = { openProfileId = it },
                onRefresh = onRefreshProfiles,
                onCreate = onCreateProfile,
                onDuplicate = onDuplicateProfile,
            )
            ToolId.Exit -> ExitCard(state = state, onTestExit = onTestExit)
            ToolId.RoutePreview -> RoutePreviewCard(state = state)
            ToolId.Dns -> DnsCard(state = state)
            ToolId.LocalCert -> LocalCertCard(
                state = state,
                onGenerate = onGenerateLocalCert,
                onInstallIntent = onInstallLocalCa,
                onInstallWithRoot = onInstallLocalCaWithRoot,
                onExport = onExportLocalCa,
            )
            ToolId.Certificate -> CertificateCard(state = state)
            ToolId.Tcp -> TcpCard()
            ToolId.TextTool -> TextToolCard()
        }
    }
}

/** 目录页：搜索框 + 按分组排布的宫格。搜索时不分分组，直接平铺结果。 */
@Composable
private fun ToolCatalog(onOpen: (ToolId) -> Unit) {
    var query by rememberSaveable { mutableStateOf("") }
    val trimmed = query.trim()
    val searching = trimmed.isNotEmpty()
    val matches = remember(trimmed) {
        if (searching) ToolId.entries.filter { it.matches(trimmed) } else ToolId.entries.toList()
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 16.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Spacer(Modifier.height(12.dp))
        OutlinedTextField(
            value = query,
            onValueChange = { query = it },
            modifier = Modifier.fillMaxWidth().testTag("toolbox-search"),
            placeholder = { Text("搜索工具：dns、证书、tcp…") },
            leadingIcon = { Icon(Icons.Default.Search, contentDescription = null) },
            trailingIcon = {
                if (query.isNotEmpty()) {
                    IconButton(
                        onClick = { query = "" },
                        modifier = Modifier.testTag("toolbox-search-clear"),
                    ) { Icon(Icons.Default.Close, contentDescription = "清空搜索") }
                }
            },
            singleLine = true,
            shape = RoundedCornerShape(12.dp),
        )

        when {
            matches.isEmpty() -> EmptySearchResult()

            searching -> {
                Hint("找到 ${matches.size} 个工具")
                ToolGrid(items = matches, onOpen = onOpen)
            }

            else -> ToolGroup.entries.forEach { group ->
                val items = ToolId.entries.filter { it.group == group }
                if (items.isEmpty()) return@forEach
                GroupHeader(group = group, count = items.size)
                // 只有一个工具的分组直接整行铺开，比留半格空位好看。
                if (items.size == 1) {
                    ToolRowItem(tool = items.first(), onClick = { onOpen(items.first()) })
                } else {
                    ToolGrid(items = items, onOpen = onOpen)
                }
            }
        }
        Spacer(Modifier.height(16.dp))
    }
}

@Composable
private fun GroupHeader(group: ToolGroup, count: Int) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Box(
            Modifier
                .size(width = 3.dp, height = 14.dp)
                .background(MaterialTheme.colorScheme.primary, RoundedCornerShape(2.dp)),
        )
        Text(group.label, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.Bold)
        Text(
            "$count",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun EmptySearchResult() {
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
    ) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text("没有匹配的工具", style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.Bold)
            Hint("换个关键字试试：dns、证书、tcp、base64、档案")
        }
    }
}

/** 两列宫格；奇数个时最后一行留空位，不把卡片拉宽。 */
@Composable
private fun ToolGrid(items: List<ToolId>, onOpen: (ToolId) -> Unit) {
    items.chunked(2).forEach { pair ->
        Row(
            modifier = Modifier.fillMaxWidth().height(IntrinsicSize.Min),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            pair.forEach { tool ->
                ToolGridItem(
                    tool = tool,
                    onClick = { onOpen(tool) },
                    modifier = Modifier.weight(1f),
                )
            }
            if (pair.size == 1) Spacer(Modifier.weight(1f))
        }
    }
}

@Composable
private fun ToolGridItem(tool: ToolId, onClick: () -> Unit, modifier: Modifier = Modifier) {
    Card(
        onClick = onClick,
        modifier = modifier.fillMaxHeight().testTag("tool-${tool.name.lowercase()}"),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
    ) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(14.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            ToolIcon(tool.icon)
            Text(
                tool.title,
                style = MaterialTheme.typography.titleSmall,
                fontWeight = FontWeight.Bold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                tool.summary,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                minLines = 2,
                maxLines = 3,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/** 分组里只有一个工具时用整行条目：图标在左、说明在右，比宫格更省地方。 */
@Composable
private fun ToolRowItem(tool: ToolId, onClick: () -> Unit) {
    Card(
        onClick = onClick,
        modifier = Modifier.fillMaxWidth().testTag("tool-${tool.name.lowercase()}"),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(14.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            ToolIcon(tool.icon)
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(
                    tool.title,
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.Bold,
                )
                Text(
                    tool.summary,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Icon(
                Icons.AutoMirrored.Filled.KeyboardArrowRight,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun ToolIcon(icon: ImageVector) {
    Box(
        modifier = Modifier
            .size(38.dp)
            .background(MaterialTheme.colorScheme.primaryContainer, RoundedCornerShape(10.dp)),
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            icon,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onPrimaryContainer,
            modifier = Modifier.size(21.dp),
        )
    }
}

/**
 * 工具详情页的外壳：一条「← 返回目录」的窄栏 + 可滚动内容。
 *
 * 工具名放在栏里，卡片自己不再重复标题，省掉两行一模一样的字。
 */
@Composable
private fun ToolDetail(title: String, onBack: () -> Unit, content: @Composable ColumnScope.() -> Unit) {
    Column(Modifier.fillMaxSize()) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(start = 4.dp, end = 16.dp, top = 2.dp, bottom = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            IconButton(onClick = onBack, modifier = Modifier.testTag("tool-back")) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "返回工具目录")
            }
            Text(
                title,
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        HorizontalDivider()
        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Spacer(Modifier.height(12.dp))
            content()
            Spacer(Modifier.height(16.dp))
        }
    }
}

/**
 * 工具箱详情页里统一的卡片外壳：一句说明 + 内容。
 *
 * 标题不在这里 —— 详情页顶栏已经写了工具名，卡片再写一遍就是两行一样的字。
 */
@Composable
private fun ToolCard(
    subtitle: String,
    content: @Composable ColumnScope.() -> Unit,
) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
    ) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text(
                subtitle,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            content()
        }
    }
}

@Composable
private fun Hint(text: String, warn: Boolean = false) {
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = if (warn) {
            MaterialTheme.colorScheme.tertiary
        } else {
            MaterialTheme.colorScheme.onSurfaceVariant
        },
    )
}

/** 正在跑的按钮内容：转圈 + 文案，避免用户重复点。 */
@Composable
private fun BusyButtonContent(busy: Boolean, idle: String, working: String) {
    if (busy) {
        CircularProgressIndicator(
            modifier = Modifier.size(16.dp),
            strokeWidth = 2.dp,
            color = MaterialTheme.colorScheme.onPrimary,
        )
        Spacer(Modifier.width(6.dp))
    }
    Text(if (busy) working else idle)
}

// ---------------------------------------------------------------------------
// 用户档案（数据隔离）
// ---------------------------------------------------------------------------

/** 档案详情页里的对话框；null 表示没有打开的框。 */
private sealed interface ProfileDialog {
    data class Rename(val profile: ProfileSummary) : ProfileDialog
    data class Delete(val profile: ProfileSummary) : ProfileDialog
}

/**
 * 用户档案：一个档案 = 一整套独立的用户数据目录 **+ 一份「接管哪些 App」的名单**。
 *
 * 分两段：上面是已有档案（**点进去**才切换 / 改名 / 调名单），下面是新建 ——
 * 先勾 App、再起名，最后「添加」才会真的多出一个档案。
 */
@Composable
private fun ProfilesCard(
    profiles: List<ProfileSummary>,
    installedApps: List<InstalledApp>,
    onOpenProfile: (String) -> Unit,
    onRefresh: () -> Unit,
    onCreate: (String, List<String>) -> Unit,
    onDuplicate: () -> Unit,
) {
    val full = profiles.size >= MAX_PROFILES

    ToolCard(
        subtitle = "每个档案一整套完全独立的用户数据：节点、订阅、规则、设置、本地 DNS、证书、" +
            "内核缓存各自一个文件夹，互不影响 —— 相当于浏览器的多个 userdir。点某个档案进去才能切换。",
    ) {
        if (profiles.isEmpty()) {
            Hint("正在读取档案…")
        }
        profiles.forEachIndexed { index, profile ->
            if (index > 0) HorizontalDivider()
            ProfileRow(
                profile = profile,
                appSummary = profileAppSummary(profile, installedApps),
                onClick = { onOpenProfile(profile.id) },
            )
        }
        HorizontalDivider()
        Row(
            modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            OutlinedButton(
                onClick = onDuplicate,
                enabled = !full && profiles.isNotEmpty(),
                modifier = Modifier.testTag("toolbox-profile-duplicate"),
            ) { Text("分身当前档案") }
            TextButton(onClick = onRefresh) { Text("刷新") }
        }
        if (full) Hint("档案数量已达上限 $MAX_PROFILES 个。", warn = true)
        Hint("「分身」会把当前档案整套复制过去（含本地证书与 App 名单），适合「一个机场账号一份档案」这种用法。")
    }

    NewProfileCard(profiles = profiles, installedApps = installedApps, onCreate = onCreate)
}

/** 新建档案：先勾选这个档案要接管的 App（可多选），再起名，最后点「添加」。 */
@Composable
private fun NewProfileCard(
    profiles: List<ProfileSummary>,
    installedApps: List<InstalledApp>,
    onCreate: (String, List<String>) -> Unit,
) {
    var name by rememberSaveable { mutableStateOf("") }
    var selected by rememberSaveable { mutableStateOf(listOf<String>()) }
    val error = profileNameError(name, profiles.map(ProfileSummary::name))
    val showError = error != null && name.isNotBlank()
    val full = profiles.size >= MAX_PROFILES
    val dirty = name.isNotBlank() || selected.isNotEmpty()

    ToolCard(
        subtitle = "勾选这个档案要接管的 App（可多选；一个都不勾 = 接管全部 App），起个名字，再点「添加」。",
    ) {
        OutlinedTextField(
            value = name,
            onValueChange = { name = it.take(PROFILE_NAME_MAX_LENGTH) },
            modifier = Modifier.fillMaxWidth().testTag("toolbox-profile-name-input"),
            label = { Text("档案名称") },
            placeholder = { Text("工作 / 家里 / 机场 A") },
            singleLine = true,
            isError = showError,
        )
        if (showError) Hint(error, warn = true)
        AppPicker(
            apps = installedApps,
            selected = selected,
            onToggle = { pkg ->
                selected = if (pkg in selected) selected - pkg else selected + pkg
            },
        )
        Hint(
            if (selected.isEmpty()) {
                "未勾选 App：这个档案会接管全部 App。"
            } else {
                "已选 ${selected.size} 个：${appLabels(installedApps, selected)}"
            },
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                onClick = {
                    onCreate(sanitizeProfileName(name), selected)
                    name = ""
                    selected = emptyList()
                },
                enabled = !full && !showError && name.isNotBlank(),
                modifier = Modifier.testTag("toolbox-profile-create"),
            ) { Text("添加") }
            TextButton(
                onClick = { name = ""; selected = emptyList() },
                enabled = dirty,
            ) { Text("清空") }
        }
        if (full) Hint("档案数量已达上限 $MAX_PROFILES 个，先删掉一个再建。", warn = true)
    }
}

/**
 * 档案详情：点进来之后才做改动。
 *
 * 两块 —— 上面是这个档案本身（切换 / 重命名 / 删除），下面是它接管的 App 名单。
 * 名单跟着档案走，所以「切档案」的同时被接管的 App 一起换。
 */
@Composable
private fun ProfileDetailCard(
    profile: ProfileSummary,
    otherNames: List<String>,
    installedApps: List<InstalledApp>,
    onSwitch: () -> Unit,
    onRename: (String) -> Unit,
    onDelete: () -> Unit,
    onSavePackages: (List<String>) -> Unit,
) {
    var dialog by remember { mutableStateOf<ProfileDialog?>(null) }
    // 以档案当前保存的名单为基准；保存后档案列表刷新，key 变了就自动回到「未修改」。
    var selected by remember(profile.id, profile.packages) { mutableStateOf(profile.packages) }
    val dirty = selected.toSet() != profile.packages.toSet()
    val canDelete = !profile.active && profile.id != DEFAULT_PROFILE_ID

    ToolCard(subtitle = "这个档案独立的一整套数据，都在这一个目录里。") {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(profile.name, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
            ActiveBadge(profile.active)
        }
        Text(
            "节点 ${profile.nodeCount} · 订阅 ${profile.subscriptionCount} · 规则 ${profile.ruleCount}",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            profile.dataDir,
            style = MaterialTheme.typography.bodySmall,
            fontFamily = FontFamily.Monospace,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        HorizontalDivider()
        if (profile.active) {
            Hint("这个档案正在使用中。想切到别的档案，回到档案列表点那一个。")
        } else {
            Button(
                onClick = onSwitch,
                modifier = Modifier.testTag("toolbox-profile-switch-${profile.id}"),
            ) { Text("切换到它") }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            TextButton(onClick = { dialog = ProfileDialog.Rename(profile) }) { Text("重命名") }
            TextButton(
                onClick = { dialog = ProfileDialog.Delete(profile) },
                enabled = canDelete,
                modifier = Modifier.testTag("toolbox-profile-delete"),
            ) { Text("删除", color = MaterialTheme.colorScheme.error) }
        }
    }

    ToolCard(
        subtitle = "只接管勾选的 App：名单外的 App 完全不进 TUN，也不受这个档案的节点与规则影响。" +
            "名单跟着档案走 —— 切档时被接管的 App 一起换。改完要重启接管才生效。",
    ) {
        AppPicker(
            apps = installedApps,
            selected = selected,
            onToggle = { pkg ->
                selected = if (pkg in selected) selected - pkg else selected + pkg
            },
        )
        Hint(
            if (selected.isEmpty()) {
                "未勾选 App：这个档案接管全部 App。"
            } else {
                "已选 ${selected.size} 个：${appLabels(installedApps, selected)}"
            },
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                onClick = { onSavePackages(selected) },
                enabled = dirty,
                modifier = Modifier.testTag("toolbox-profile-save-apps"),
            ) { Text("保存 App 名单") }
            OutlinedButton(
                onClick = { selected = profile.packages },
                enabled = dirty,
            ) { Text("还原") }
        }
    }

    when (val current = dialog) {
        null -> Unit

        is ProfileDialog.Rename -> ProfileNameDialog(
            title = "重命名档案",
            hint = "只改显示名，数据目录与 App 名单都不动。",
            initial = current.profile.name,
            confirmLabel = "保存",
            existingNames = otherNames,
            onDismiss = { dialog = null },
            onConfirm = { onRename(it); dialog = null },
        )

        is ProfileDialog.Delete -> AlertDialog(
            onDismissRequest = { dialog = null },
            title = { Text("删除档案「${current.profile.name}」？") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Hint("该档案的配置、本地证书与内核缓存目录会一并删除，不可恢复。", warn = true)
                    Text(
                        current.profile.dataDir,
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = FontFamily.Monospace,
                    )
                }
            },
            confirmButton = {
                TextButton(
                    onClick = { onDelete(); dialog = null },
                    modifier = Modifier.testTag("toolbox-profile-delete-confirm"),
                ) { Text("删除", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = { TextButton(onClick = { dialog = null }) { Text("取消") } },
        )
    }
}

/** 档案列表里的一行：整行可点，点进去才是详情（切换 / 改名 / 调 App 名单都在里面）。 */
@Composable
private fun ProfileRow(profile: ProfileSummary, appSummary: String, onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(10.dp))
            .clickable(onClick = onClick)
            .padding(vertical = 10.dp, horizontal = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(profile.name, style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.Bold)
                ActiveBadge(profile.active)
            }
            // App 范围是这份档案最要紧的属性（决定它管谁），用强调色摆出来。
            Text(
                appSummary,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.primary,
                fontWeight = FontWeight.Medium,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                "节点 ${profile.nodeCount} · 订阅 ${profile.subscriptionCount} · 规则 ${profile.ruleCount}",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                profile.dataDir,
                style = MaterialTheme.typography.bodySmall,
                fontFamily = FontFamily.Monospace,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Icon(
            Icons.AutoMirrored.Filled.KeyboardArrowRight,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun ActiveBadge(active: Boolean) {
    if (!active) return
    Text(
        "使用中",
        modifier = Modifier
            .background(MaterialTheme.colorScheme.primaryContainer, RoundedCornerShape(6.dp))
            .padding(horizontal = 8.dp, vertical = 2.dp),
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onPrimaryContainer,
        fontWeight = FontWeight.Bold,
    )
}

// ---------------------------------------------------------------------------
// App 选择：搜索 + 九宫格
// ---------------------------------------------------------------------------

/**
 * App 九宫格选择器。
 *
 * 用宫格不用列表：挑 App 靠的是「认图标」，一眼扫过去比读一列文字快得多。
 * 高度写死是为了不和外面那层纵向滚动打架（内层自己滚）。
 */
@Composable
private fun AppPicker(
    apps: List<InstalledApp>,
    selected: List<String>,
    onToggle: (String) -> Unit,
) {
    var query by rememberSaveable { mutableStateOf("") }
    val visible = remember(apps, query) {
        val needle = query.trim()
        if (needle.isEmpty()) {
            apps
        } else {
            apps.filter {
                it.label.contains(needle, ignoreCase = true) ||
                    it.packageName.contains(needle, ignoreCase = true)
            }
        }
    }

    OutlinedTextField(
        value = query,
        onValueChange = { query = it },
        modifier = Modifier.fillMaxWidth().testTag("toolbox-app-search"),
        label = { Text("搜索应用") },
        placeholder = { Text("微信 / chrome / com.tencent") },
        leadingIcon = { Icon(Icons.Default.Search, contentDescription = null) },
        singleLine = true,
    )
    when {
        apps.isEmpty() -> Hint("正在读取已安装应用…")
        visible.isEmpty() -> Hint("没有匹配的应用。")
        else -> LazyVerticalGrid(
            columns = GridCells.Fixed(4),
            modifier = Modifier.fillMaxWidth().height(288.dp),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            items(visible, key = InstalledApp::packageName) { app ->
                AppGridCell(
                    app = app,
                    checked = app.packageName in selected,
                    onClick = { onToggle(app.packageName) },
                )
            }
        }
    }
}

@Composable
private fun AppGridCell(app: InstalledApp, checked: Boolean, onClick: () -> Unit) {
    val icon = rememberAppIcon(app.packageName)
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(10.dp))
            .background(
                if (checked) {
                    MaterialTheme.colorScheme.primaryContainer
                } else {
                    MaterialTheme.colorScheme.surfaceVariant
                },
            )
            .clickable(onClick = onClick)
            .padding(vertical = 8.dp, horizontal = 4.dp)
            .testTag("toolbox-app-${app.packageName}"),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Box(modifier = Modifier.size(42.dp), contentAlignment = Alignment.Center) {
            if (icon != null) {
                Image(
                    bitmap = icon,
                    contentDescription = null,
                    modifier = Modifier.size(40.dp),
                )
            } else {
                // 拿不到图标（很少见）就退回首字母，至少占位一致，不会跳版。
                Text(
                    app.label.take(1),
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.Bold,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (checked) {
                Box(
                    modifier = Modifier
                        .align(Alignment.TopEnd)
                        .size(16.dp)
                        .background(MaterialTheme.colorScheme.primary, CircleShape),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(
                        Icons.Default.Check,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.onPrimary,
                        modifier = Modifier.size(11.dp),
                    )
                }
            }
        }
        Text(
            app.label,
            style = MaterialTheme.typography.labelSmall,
            textAlign = TextAlign.Center,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** 应用图标加载。包名一多就得异步，且按格子懒加载（宫格只组合可见项）。 */
@Composable
private fun rememberAppIcon(packageName: String): ImageBitmap? {
    val context = LocalContext.current
    return produceState<ImageBitmap?>(initialValue = null, packageName) {
        value = withContext(Dispatchers.IO) {
            runCatching {
                context.packageManager.getApplicationIcon(packageName)
                    .toBitmap(width = 96, height = 96)
                    .asImageBitmap()
            }.getOrNull()
        }
    }.value
}

/** 包名 → 应用名；查不到（已卸载，或名单里手写过包名）就原样显示包名。 */
private fun appLabels(apps: List<InstalledApp>, packages: List<String>): String =
    packages.map { pkg -> apps.firstOrNull { it.packageName == pkg }?.label ?: pkg }
        .joinToString("、")

/** 档案列表里那句 App 范围摘要。 */
private fun profileAppSummary(profile: ProfileSummary, apps: List<InstalledApp>): String =
    if (profile.packages.isEmpty()) {
        "接管全部 App"
    } else {
        "只接管 ${profile.packages.size} 个：${appLabels(apps, profile.packages)}"
    }

/** 新建 / 重命名共用的取名对话框：实时校验，名字重复或为空时确认键直接禁用。 */
@Composable
private fun ProfileNameDialog(
    title: String,
    hint: String,
    initial: String,
    confirmLabel: String,
    existingNames: List<String>,
    onDismiss: () -> Unit,
    onConfirm: (String) -> Unit,
) {
    var value by remember { mutableStateOf(initial) }
    val error = profileNameError(value, existingNames)
    val showError = error != null && value.isNotBlank()

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Hint(hint)
                OutlinedTextField(
                    value = value,
                    onValueChange = { value = it.take(PROFILE_NAME_MAX_LENGTH) },
                    modifier = Modifier.fillMaxWidth().testTag("toolbox-profile-name-input"),
                    label = { Text("档案名称") },
                    placeholder = { Text("工作 / 家里 / 机场 A") },
                    singleLine = true,
                    isError = showError,
                )
                if (showError) Hint(error, warn = true)
            }
        },
        confirmButton = {
            TextButton(
                onClick = { onConfirm(sanitizeProfileName(value)) },
                enabled = error == null,
                modifier = Modifier.testTag("toolbox-profile-name-confirm"),
            ) { Text(confirmLabel) }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("取消") } },
    )
}

// ---------------------------------------------------------------------------
// 出口检测
// ---------------------------------------------------------------------------

@Composable
private fun ExitCard(state: AppState, onTestExit: () -> Unit) {
    val checking = state.exitStatus == ExitCheckStatus.Checking
    ToolCard(
        subtitle = "分别从本机直连和所选节点出去各查一次出口 IP，用来确认节点到底有没有生效。",
    ) {
        ExitLine("直连出口", state.directExit)
        ExitLine("代理出口", state.proxyExit)
        if (state.proxyExit.ip.isBlank() && !checking) {
            Hint("代理出口为空：需要先启动接管并选中一个节点。")
        }
        if (state.exitMessage.isNotBlank()) Hint(state.exitMessage, warn = true)
        HorizontalDivider()
        Button(
            onClick = onTestExit,
            enabled = !checking,
            modifier = Modifier.testTag("toolbox-exit-test"),
        ) {
            Icon(Icons.Default.Refresh, contentDescription = null, modifier = Modifier.size(18.dp))
            Spacer(Modifier.width(6.dp))
            Text(if (checking) "检测中…" else "重新检测")
        }
    }
}

@Composable
private fun ExitLine(label: String, info: ExitInfo) {
    Row(verticalAlignment = Alignment.Top) {
        Text(
            label,
            modifier = Modifier.width(76.dp),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Column(Modifier.weight(1f)) {
            Text(
                info.ip.ifBlank { "—" },
                style = MaterialTheme.typography.bodyMedium,
                fontFamily = FontFamily.Monospace,
                fontWeight = FontWeight.Medium,
            )
            if (info.location.isNotBlank()) {
                Text(
                    info.location,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 规则命中预判
// ---------------------------------------------------------------------------

@Composable
private fun RoutePreviewCard(state: AppState) {
    var query by rememberSaveable { mutableStateOf("") }
    var preview by remember { mutableStateOf<NetworkTools.RoutePreview?>(null) }

    ToolCard(
        subtitle = "按当前模式、常用站点与自定义规则的匹配顺序，判断某个域名 / IP 会从哪条出站走。",
    ) {
        OutlinedTextField(
            value = query,
            onValueChange = { query = it },
            modifier = Modifier.fillMaxWidth().testTag("toolbox-route-input"),
            label = { Text("域名或 IP") },
            placeholder = { Text("chatgpt.com") },
            singleLine = true,
        )
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Button(
                onClick = { preview = NetworkTools.previewRoute(state, query) },
                modifier = Modifier.testTag("toolbox-route-run"),
            ) {
                Icon(Icons.Default.PlayArrow, contentDescription = null, modifier = Modifier.size(18.dp))
                Spacer(Modifier.width(6.dp))
                Text("预判")
            }
            OutlinedButton(
                onClick = { query = ""; preview = null },
                enabled = query.isNotBlank() || preview != null,
            ) { Text("清空") }
        }
        preview?.let { result ->
            OutboundBadge(result.outbound)
            Text(result.reason, style = MaterialTheme.typography.bodySmall)
            result.notes.forEach { Hint("注意：$it", warn = true) }
        }
    }
}

@Composable
private fun OutboundBadge(outbound: NetworkTools.Outbound) {
    val background = when (outbound) {
        NetworkTools.Outbound.Proxy -> MaterialTheme.colorScheme.secondaryContainer
        NetworkTools.Outbound.Smart -> MaterialTheme.colorScheme.primaryContainer
        NetworkTools.Outbound.Direct -> MaterialTheme.colorScheme.surfaceVariant
    }
    val foreground = when (outbound) {
        NetworkTools.Outbound.Proxy -> MaterialTheme.colorScheme.onSecondaryContainer
        NetworkTools.Outbound.Smart -> MaterialTheme.colorScheme.onPrimaryContainer
        NetworkTools.Outbound.Direct -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    Text(
        "→ ${outbound.label}",
        modifier = Modifier
            .testTag("toolbox-route-outbound")
            .background(background, RoundedCornerShape(6.dp))
            .padding(horizontal = 10.dp, vertical = 5.dp),
        color = foreground,
        fontWeight = FontWeight.Bold,
        style = MaterialTheme.typography.bodyMedium,
    )
}

// ---------------------------------------------------------------------------
// DNS 查询
// ---------------------------------------------------------------------------

@Composable
private fun DnsCard(state: AppState) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var name by rememberSaveable { mutableStateOf("") }
    var type by rememberSaveable { mutableStateOf("A") }
    var viaProxy by rememberSaveable { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var result by remember { mutableStateOf<NetworkTools.DnsResult?>(null) }
    val proxyPort = state.localProxyActualPort.takeIf { it > 0 }

    ToolCard(
        subtitle = "走 DoH（dns.google）查询，可勾选从本地代理端口出去，用来对比直连与代理的解析结果。",
    ) {
        OutlinedTextField(
            value = name,
            onValueChange = { name = it },
            modifier = Modifier.fillMaxWidth().testTag("toolbox-dns-input"),
            label = { Text("域名") },
            placeholder = { Text("example.com") },
            singleLine = true,
        )
        // 命中本地映射时先把话说清楚：这张卡片是直接问 dns.google 的，
        // 走的是 DoH，不经过内核，所以结果**不会**体现本地映射——免得用户
        // 看到 DoH 和浏览器里的解析结果不一样，以为映射没生效。
        localMappingFor(state, name)?.let { entry ->
            Hint(
                "本地 DNS 映射：${entry.domain} → ${entry.address}。" +
                    "注意本卡片是直连 dns.google 查询，不受映射影响；映射只作用于内核（被 TUN 接管的应用）。",
            )
        }
        Row(
            modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            NetworkTools.dnsRecordTypes.forEach { item ->
                FilterChip(
                    selected = type == item,
                    onClick = { type = item },
                    label = { Text(item) },
                    modifier = Modifier.testTag("toolbox-dns-type-$item"),
                )
            }
        }
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Column(Modifier.weight(1f)) {
                Text("经本地代理查询", style = MaterialTheme.typography.bodyMedium)
                Hint(
                    if (proxyPort != null) {
                        "从 127.0.0.1:$proxyPort 出去，解析结果来自节点侧"
                    } else {
                        "本地代理未监听，先在「设置」里打开本地代理端口"
                    },
                )
            }
            Switch(
                checked = viaProxy && proxyPort != null,
                enabled = proxyPort != null && !busy,
                onCheckedChange = { viaProxy = it },
                modifier = Modifier.testTag("toolbox-dns-via-proxy"),
            )
        }
        Button(
            onClick = {
                busy = true
                result = null
                scope.launch {
                    val outcome = withContext(Dispatchers.IO) {
                        runCatching {
                            NetworkTools.queryDns(
                                name = name,
                                type = type,
                                proxyPort = if (viaProxy) proxyPort else null,
                            )
                        }
                    }
                    busy = false
                    outcome
                        .onSuccess { result = it }
                        .onFailure {
                            Toast.makeText(context, it.message ?: "DNS 查询失败", Toast.LENGTH_SHORT).show()
                        }
                }
            },
            enabled = !busy && name.isNotBlank(),
            modifier = Modifier.testTag("toolbox-dns-run"),
        ) {
            BusyButtonContent(busy = busy, idle = "查询", working = "查询中…")
        }
        result?.let { outcome ->
            HorizontalDivider()
            Hint(outcome.detail)
            outcome.records.forEach { record ->
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        record.type,
                        modifier = Modifier.width(56.dp),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.secondary,
                        fontWeight = FontWeight.Bold,
                    )
                    Text(
                        record.value,
                        modifier = Modifier.weight(1f),
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = FontFamily.Monospace,
                    )
                    if (record.ttl > 0) {
                        Text(
                            "${record.ttl}s",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 本地证书
// ---------------------------------------------------------------------------

/**
 * 本地 CA 与叶证书的生成入口。
 *
 * 「局域网里用域名访问、HTTPS 还不报红」的另一半：本地 DNS 映射负责解析，
 * 这里负责证书 —— 按当前映射签一套 CA + 叶证书（叶 397 天，Chrome 的 398 天上限
 * 对私有根同样生效），信任装不装由用户决定。状态字段来自 Rust 同一套规则，
 * 两端行为一致。
 */
@Composable
private fun LocalCertCard(
    state: AppState,
    onGenerate: () -> Unit,
    onInstallIntent: () -> Intent?,
    onInstallWithRoot: () -> Unit,
    onExport: () -> Unit,
) {
    val context = LocalContext.current
    val cert = state.localCert

    // 系统安装 CA 的对话框要走 activity result 才能弹出来。
    val installLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.StartActivityForResult(),
    ) { _ -> /* 装没装成要看系统证书列表，这里不猜 */ }

    ToolCard(
        subtitle = "按当前映射自动签一套 CA 与叶证书，浏览器要认还得装进系统信任库。",
    ) {
        if (cert.names.isEmpty()) {
            Hint("还没有本地 DNS 映射 —— 先去设置里加一条，再来生成证书。", warn = true)
        }
        CertField("根证书", if (cert.caExists) "已生成 · ${cert.caValidityDays} 天" else "未生成")
        CertField(
            "叶证书",
            when {
                !cert.leafExists -> "未生成"
                cert.namesInSync ->
                    "覆盖 ${cert.names.size} 个名字 · 至 ${cert.leafNotAfter}（剩 ${cert.leafDaysLeft} 天）"
                else -> "证书里 ${cert.certifiedNames.size} 个名字，映射需要 ${cert.names.size} 个 —— 保存映射或重新生成"
            },
        )
        if (cert.names.isNotEmpty()) {
            CertField("覆盖名字", cert.names.joinToString("、"))
        }
        CertField("存放位置", cert.dir.ifEmpty { "生成后位于应用私有目录 local-certs/" })
        Hint(
            "浏览器报不报红取决于系统信不信任这套 CA：免 root 走系统安装对话框（需要锁屏），" +
                "有 root 可直接放进系统证书目录。",
        )

        Row(
            modifier = Modifier.fillMaxWidth().testTag("toolbox-localcert-actions"),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Button(
                onClick = onGenerate,
                modifier = Modifier.testTag("toolbox-localcert-generate"),
            ) {
                BusyButtonContent(busy = false, idle = "生成或更新证书", working = "")
            }
        }
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedButton(
                onClick = {
                    // 意图可能为 null（还没生成根证书），提示已经由 ViewModel 发出。
                    val intent = onInstallIntent()
                    if (intent != null) installLauncher.launch(intent)
                },
                enabled = cert.caExists,
                modifier = Modifier.testTag("toolbox-localcert-install"),
            ) {
                Text("安装 CA")
            }
            OutlinedButton(
                onClick = onExport,
                enabled = cert.caExists,
                modifier = Modifier.testTag("toolbox-localcert-export"),
            ) {
                Text("导出 ca.crt")
            }
            TextButton(
                onClick = onInstallWithRoot,
                enabled = cert.caExists,
            ) {
                Text("Root 安装到系统库")
            }
        }
    }
}

// 证书检测
// ---------------------------------------------------------------------------

/**
 * 「局域网里用域名访问、证书还能过校验」由两件事组成：域名解析要对（本地 DNS
 * 映射），证书链要被信任且名字要对得上。这张卡片负责确认后半段——判定用的是
 * 平台默认信任库，结论等同于「浏览器打开会不会报红」。
 */
@Composable
private fun CertificateCard(state: AppState) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var target by rememberSaveable { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var result by remember { mutableStateOf<NetworkTools.CertificateProbe?>(null) }

    ToolCard(
        subtitle = "对「域名:端口」做一次真实 TLS 握手，看系统会不会认这张证书。",
    ) {
        OutlinedTextField(
            value = target,
            onValueChange = { target = it },
            modifier = Modifier.fillMaxWidth().testTag("toolbox-cert-input"),
            label = { Text("域名或 域名:端口") },
            placeholder = { Text("nas.lan 或 nas.lan:8443") },
            singleLine = true,
        )
        localMappingFor(state, NetworkTools.parseHostPort(target).first)?.let { entry ->
            Hint("该域名有本地映射：${entry.domain} → ${entry.address}")
        }
        Button(
            onClick = {
                busy = true
                result = null
                val (host, port) = NetworkTools.parseHostPort(target)
                scope.launch {
                    val outcome = withContext(Dispatchers.IO) {
                        runCatching { NetworkTools.probeCertificate(host, port) }
                    }
                    busy = false
                    outcome
                        .onSuccess { result = it }
                        .onFailure {
                            Toast.makeText(context, it.message ?: "证书检测失败", Toast.LENGTH_SHORT)
                                .show()
                        }
                }
            },
            enabled = !busy && target.isNotBlank(),
            modifier = Modifier.testTag("toolbox-cert-run"),
        ) {
            BusyButtonContent(busy = busy, idle = "检测", working = "检测中…")
        }
        result?.let { probe ->
            HorizontalDivider()
            if (probe.resolvedAddresses.isNotEmpty()) {
                Text(
                    "${probe.host}:${probe.port} → ${probe.resolvedAddresses.joinToString(", ")}",
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                )
            }
            CertVerdict("系统信任", probe.trustedBySystem)
            if (probe.reachable) CertVerdict("域名匹配", probe.hostnameVerified)
            Hint(probe.detail, warn = !probe.trustedBySystem || !probe.hostnameVerified)
            probe.chain.forEachIndexed { index, certificate ->
                CertDetail(index = index, certificate = certificate)
            }
        }
    }
}

/** 查询名归一后去映射表里精确匹配——和内核那边 hosts 服务器的匹配方式一致。 */
internal fun localMappingFor(state: AppState, rawName: String): LocalDnsEntry? {
    val target = normalizeLocalDnsDomain(rawName)
    if (target.isEmpty()) return null
    return state.localDnsEntries.firstOrNull { it.domain == target }
}

@Composable
private fun CertVerdict(label: String, ok: Boolean) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            if (ok) "✓" else "✗",
            fontWeight = FontWeight.Bold,
            color = if (ok) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.error,
        )
        Text(label, style = MaterialTheme.typography.bodyMedium)
    }
}

@Composable
private fun CertDetail(index: Int, certificate: NetworkTools.CertificateInfo) {
    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        Text(
            if (index == 0) "服务器证书" else "中间证书 $index",
            style = MaterialTheme.typography.bodySmall,
            fontWeight = FontWeight.SemiBold,
        )
        CertField("主体", certificate.subject)
        CertField("签发者", certificate.issuer + if (certificate.selfSigned) "（自签）" else "")
        CertField("有效期", "${certificate.notBefore} ~ ${certificate.notAfter}")
        CertField("签名算法", certificate.signatureAlgorithm)
        if (certificate.alternativeNames.isNotEmpty()) {
            CertField("覆盖域名", certificate.alternativeNames.joinToString(", "))
        }
    }
}

@Composable
private fun CertField(label: String, value: String) {
    Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            label,
            modifier = Modifier.width(64.dp),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            value,
            modifier = Modifier.weight(1f),
            style = MaterialTheme.typography.bodySmall,
        )
    }
}

// ---------------------------------------------------------------------------
// TCP 连通性
// ---------------------------------------------------------------------------

@Composable
private fun TcpCard() {
    val scope = rememberCoroutineScope()
    var host by rememberSaveable { mutableStateOf("") }
    var port by rememberSaveable { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var result by remember { mutableStateOf<NetworkTools.TcpProbeResult?>(null) }

    ToolCard(
        subtitle = "对 host:port 做一次直连 TCP 握手（不经代理），用来区分「端口不通」和「代理没生效」。",
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = host,
                onValueChange = { host = it },
                modifier = Modifier.weight(1f).testTag("toolbox-tcp-host"),
                label = { Text("主机") },
                placeholder = { Text("1.1.1.1") },
                singleLine = true,
            )
            OutlinedTextField(
                value = port,
                onValueChange = { value -> port = value.filter(Char::isDigit).take(5) },
                modifier = Modifier.width(104.dp).testTag("toolbox-tcp-port"),
                label = { Text("端口") },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
            )
        }
        Button(
            onClick = {
                busy = true
                result = null
                scope.launch {
                    val outcome = withContext(Dispatchers.IO) {
                        NetworkTools.probeTcp(host, port.toIntOrNull() ?: -1)
                    }
                    busy = false
                    result = outcome
                }
            },
            enabled = !busy && host.isNotBlank() && port.isNotBlank(),
            modifier = Modifier.testTag("toolbox-tcp-run"),
        ) {
            BusyButtonContent(busy = busy, idle = "测试", working = "测试中…")
        }
        result?.let { outcome ->
            HorizontalDivider()
            Text(
                if (outcome.reachable) "✓ 可连接" else "✗ 不可连接",
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.Bold,
                color = if (outcome.reachable) {
                    MaterialTheme.colorScheme.primary
                } else {
                    MaterialTheme.colorScheme.error
                },
            )
            Hint(outcome.detail)
        }
    }
}

// ---------------------------------------------------------------------------
// 编解码 / 链接解析
// ---------------------------------------------------------------------------

internal enum class TextTool(val label: String) {
    Base64Encode("Base64 编码"),
    Base64Decode("Base64 解码"),
    UrlEncode("URL 编码"),
    UrlDecode("URL 解码"),
    Uuid("UUID"),
    ParseLink("解析节点链接"),
}

@Composable
private fun TextToolCard() {
    val context = LocalContext.current
    var tool by rememberSaveable { mutableStateOf(TextTool.Base64Encode) }
    var input by rememberSaveable { mutableStateOf("") }
    var output by remember { mutableStateOf("") }

    ToolCard(
        subtitle = "处理订阅里常见的文本：Base64 / URL 编解码、生成 UUID，以及把 ss:// vmess:// 这类分享链接拆成字段。",
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            TextTool.entries.forEach { item ->
                FilterChip(
                    selected = tool == item,
                    onClick = { tool = item; output = "" },
                    label = { Text(item.label) },
                    modifier = Modifier.testTag("toolbox-text-${item.name.lowercase()}"),
                )
            }
        }
        if (tool != TextTool.Uuid) {
            OutlinedTextField(
                value = input,
                onValueChange = { input = it },
                modifier = Modifier.fillMaxWidth().testTag("toolbox-text-input"),
                label = { Text(if (tool == TextTool.ParseLink) "分享链接" else "内容") },
                minLines = if (tool == TextTool.ParseLink) 2 else 1,
                maxLines = 5,
            )
        }
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Button(
                onClick = { output = runTextTool(tool, input) },
                enabled = tool == TextTool.Uuid || input.isNotBlank(),
                modifier = Modifier.testTag("toolbox-text-run"),
            ) { Text(if (tool == TextTool.Uuid) "生成" else "执行") }
            OutlinedButton(
                onClick = { input = ""; output = "" },
                enabled = input.isNotBlank() || output.isNotBlank(),
            ) { Text("清空") }
        }
        if (output.isNotBlank()) {
            HorizontalDivider()
            Text(
                output,
                modifier = Modifier.fillMaxWidth().testTag("toolbox-text-output"),
                style = MaterialTheme.typography.bodySmall,
                fontFamily = FontFamily.Monospace,
            )
            TextButton(onClick = { copyToClipboard(context, output) }) {
                Icon(Icons.Default.ContentCopy, contentDescription = null, modifier = Modifier.size(16.dp))
                Spacer(Modifier.width(6.dp))
                Text("复制结果")
            }
        }
    }
}

/** 纯函数，方便单测；失败时返回可读的说明而不是抛异常。 */
internal fun runTextTool(tool: TextTool, input: String): String = when (tool) {
    TextTool.Base64Encode -> NetworkTools.encodeBase64(input)
    TextTool.Base64Decode -> NetworkTools.decodeBase64(input)
        ?: "解码失败：不是合法的 Base64（已兼容 URL-safe 与缺失 padding）"
    TextTool.UrlEncode -> NetworkTools.encodeUrl(input)
    TextTool.UrlDecode -> NetworkTools.decodeUrl(input) ?: "解码失败：不是合法的 URL 编码"
    TextTool.Uuid -> NetworkTools.randomUuid()
    TextTool.ParseLink -> NetworkTools.describeShareLink(input)
}
