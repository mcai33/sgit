# sgit

sgit 帮你在 macOS、Windows 和 Linux 上接续代码工作。每台电脑克隆同一个远端仓库，登记本机目录，然后从桌面端或 CLI 拉取、提交并推送。两种入口共用同一份本机仓库清单。

## 使用流程

1. 确保每台设备上的 Git 能访问远端，当前分支已设置上游，并配置好提交身份。
2. 在桌面端点「添加仓库」，选择一个或多个目录。sgit 会递归扫描这些目录，默认勾选找到的新仓库。取消勾选不需要的仓库，再确认添加。已登记的仓库会标记为「已添加」。回到主列表后，可点击「同步」批量处理勾选的仓库。
3. 在一台设备推送，换设备后先拉取再继续编辑。新文件需要先用 `git add` 加入暂存区。

| 操作 | 行为 |
| --- | --- |
| 拉取 | 获取上游分支并快进。工作区的已跟踪改动阻止快进时，先提交或暂存到 stash。 |
| 提交并推送 | 检查远端可正常快进后，将已跟踪文件的修改和删除、以及手动暂存的新文件合成一个提交。已有本地提交时可以直接推送。 |
| 分支分叉 | 跳过该仓库，继续执行批次。用 Git 整理分叉后重试。 |
| 提交后推送失败 | 保留新提交并报告「已提交，待推送」。排除故障后重新推送即可。 |
| 移除 | 只移除清单中的登记项。 |

未跟踪且未暂存的文件保留在本机。自动提交会包含已跟踪文件的全部改动，包括同一文件部分暂存时剩余的改动。Git hooks 和签名设置照常生效。领先和落后计数基于最近一次获取的远端状态，执行同步时会重新获取当前上游分支。

## 仓库分组

点击左侧分组栏的「＋」新建分组，再把仓库卡片拖到该分组。拖到「未分组」可清除归属，也可以在仓库详情中选择分组。点击某个分组只显示该组的仓库，顶部「同步」处理当前视图中勾选的仓库。分组设置保存在本机。

## 克隆与移除

点「克隆仓库」后填入 Git 地址，选择保存目录并确认新建目录名。克隆成功后会自动加入列表。保存位置已有同名目录时，sgit 会保留现有目录。

在仓库信息里，「从列表移除」只取消登记。「从磁盘删除」需要确认完整路径，再将整个目录移到系统废纸篓或回收站。包含其他已登记仓库或与其他 Git worktree 共享元数据的目录需要先用 Git 处理。

## 桌面端同步

桌面端启动时读取本地状态。点击「同步」才会检查所选仓库的远端，并在工作区干净时拉取可以快进的更新。提交和推送需要再次确认。Git 未忽略的新文件默认不勾选，确认后未勾选的文件留在本机。遇到分叉或远端不可达时，界面列出需要处理的仓库。

## CLI

需要 Rust 1.89 或更高版本，以及可在 `PATH` 中找到的 Git 2.38 或更高版本。

```sh
cargo install --path . --locked
sgit add ~/work/project
sgit list
sgit status
sgit pull --all
sgit push project
sgit push --all
```

`pull` 和 `push` 接受一个或多个仓库 ID，或使用 `--all`。任一仓库操作失败时，CLI 仍处理剩余仓库，并最终返回非零退出码。

## 桌面端开发与打包

需要 Rust 1.90 或更高版本、Node.js 22.12 或更高版本，以及对应平台的 [Tauri 构建依赖](https://v2.tauri.app/start/prerequisites/)。

```sh
cd desktop
npm ci
npm run check
npm run tauri dev
npm run tauri build
```

桌面端调用系统 Git，使用该应用进程可访问的 Git 凭据助手和 SSH agent。安装包在目标平台构建。

## 本机配置

- macOS · `~/Library/Application Support/sgit`
- Windows · `%APPDATA%\sgit`
- Linux · `${XDG_CONFIG_HOME:-$HOME/.config}/sgit`

设置绝对路径环境变量 `SGIT_CONFIG_DIR` 可以为两个入口指定同一个自定义目录。

## 验证

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cd desktop
npm run check
npm run build
```

## GitHub 发布

推送两段式版本标签（如 `v1.0`）会运行 GitHub Actions。工作流先验证版本，运行测试，再构建 macOS Apple Silicon 和 Intel 的 DMG、Windows x64 的安装程序、Linux x64 的 DEB 和 AppImage，以及各平台的独立 CLI。全部构建完成后，安装包会一起上传到 GitHub Release。

应用和 CLI 的包版本使用三段式 SemVer，例如 `1.0.0`。标签和界面使用两段式版本，例如 `v1.0` 和 `1.0`。macOS 发布包使用临时签名，未经 Apple 公证。Windows 安装包未签名。
