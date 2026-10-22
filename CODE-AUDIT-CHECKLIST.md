# JM Boom 代码审计 Checklist

审计日期：2026-10-21。审计对象：当前工作区源码、依赖实现及构建配置。修复进度：第 1 至 12 项及第 14 项已修复，第 13 项待确认，其余 36 项待处理。

P1 表示优先处理的故障切换或数据一致性风险；P2 表示有明确触发条件的行为、可访问性或资源成本问题；P3 表示可排期的精简与风格整理。未勾选表示尚未修复或仍待确认。路径均为完整绝对路径，冒号后为定位行号。

本次以静态审查为主，运行了项目验证基线并对三个行为做了最小复现。没有执行浏览器视觉验收或并发压力测试，因此性能项描述的是代码中的成本来源及触发条件，不代表已经测得线上延迟。shadcn 允许通过 className 定制；合理布局扩展、原生语义标签和保护并发/文件完整性的检查不自动构成问题。

## 行为、状态一致性与错误处理

- [x] **01 · P1 · 通用上游请求把可重试的 HTTP 故障归为不可重试错误（已修复）**

  已修复：`decode_response` 在读取和解析正文前，将 HTTP 5xx、408、429 归为可重试的 `Http` 错误，保留状态码与请求 URL。其他非 2xx 响应尝试提取 `errorMsg`，没有合法 JSON 时使用 HTTP 错误文案，保持不可重试；仅成功响应进入原有业务信封解析及解密流程。沿用现有端点切换机制。

  验证：新增 7 个回归测试，覆盖 HTML/JSON/空正文的 HTTP 故障、普通 4xx 文案、业务错误、正常 JSON 与加密响应，并通过本地模拟 HTTP 端点验证实际故障切换。后端测试、格式检查和 Clippy 均通过，最新基线见文末。

  完整路径：[E:/ppxb/jm-boom/server/src/jm/client.rs:374](E:/ppxb/jm-boom/server/src/jm/client.rs:374)；[E:/ppxb/jm-boom/server/src/jm/client.rs:225](E:/ppxb/jm-boom/server/src/jm/client.rs:225)；[E:/ppxb/jm-boom/server/src/endpoint/tests.rs:142](E:/ppxb/jm-boom/server/src/endpoint/tests.rs:142)；[E:/ppxb/jm-boom/server/src/jm/error.rs:43](E:/ppxb/jm-boom/server/src/jm/error.rs:43)；[E:/ppxb/jm-boom/server/src/endpoint/failover.rs:38](E:/ppxb/jm-boom/server/src/endpoint/failover.rs:38)。

- [x] **02 · P1 · 删除下载任务期间仍允许恢复，存在并发删除与新 worker 竞争（已修复）**

  已修复：DownloadManager 复用现有 `KeyedLock`，按 task_id 串行化恢复、删除以及暂停/取消的完整管理流程，覆盖状态修改、持久化和 worker 停止/调度。删除完成后，等待中的恢复请求返回 NotFound；先开始的恢复或停止操作必须完成持久化，删除才会继续。不同任务仍可并行，worker 保留现有 generation 校验，不获取管理操作锁，避免删除等待 worker 时相互阻塞。

  验证：先用通知与单连接数据库控制请求交错，新增的 3 个并发回归测试在旧代码上全部失败，修复后全部通过。覆盖删除期间恢复、恢复持久化期间删除、暂停/取消持久化期间删除，并验证任务记录、manifest、文件及 worker 清理和不同任务的并发性。

  完整路径：[E:/ppxb/jm-boom/server/src/download/manager.rs:198](E:/ppxb/jm-boom/server/src/download/manager.rs:198)；[E:/ppxb/jm-boom/server/src/download/manager.rs:228](E:/ppxb/jm-boom/server/src/download/manager.rs:228)；[E:/ppxb/jm-boom/server/src/download/manager.rs:262](E:/ppxb/jm-boom/server/src/download/manager.rs:262)；[E:/ppxb/jm-boom/server/src/download/manager.rs:598](E:/ppxb/jm-boom/server/src/download/manager.rs:598)；[E:/ppxb/jm-boom/server/src/keyed_lock.rs:28](E:/ppxb/jm-boom/server/src/keyed_lock.rs:28)。

- [x] **03 · P2 · 暂停、取消和删除不能及时打断正在进行的上游网络请求（已修复）**

  已修复：worker 获取章节，以及共享页面物化服务获取章节/下载图片时，通过 `tokio::select!` 优先响应取消通知；取消会结束当前网络等待及其重试流程，释放页面锁和相关并发许可。取消后不再派发后续页面 job，已进入解码或缓存写入的 job 正常收尾，保留 generation、single-flight、解码限额和文件完整性保护。

  验证：新增 3 个回归测试，旧代码均因取消后继续等待而超时，修复后通过。真实 HTTP 模拟覆盖章节、图片域名查询、图片下载三个阻塞阶段下的暂停/取消/删除，共 9 种组合，并验证暂停后恢复、任务名额释放及删除清理。共享页面测试验证取消等待者不影响请求发起者；取消下载发起者后，读者可接手重新获取页面，完成解扰和缓存写入。测试以 1 秒为退出上限，不依赖上游网络超时。

  完整路径：[E:/ppxb/jm-boom/server/src/download/worker.rs:54](E:/ppxb/jm-boom/server/src/download/worker.rs:54)；[E:/ppxb/jm-boom/server/src/download/worker.rs:91](E:/ppxb/jm-boom/server/src/download/worker.rs:91)；[E:/ppxb/jm-boom/server/src/download/scheduler.rs:114](E:/ppxb/jm-boom/server/src/download/scheduler.rs:114)；[E:/ppxb/jm-boom/server/src/page_materializer.rs:145](E:/ppxb/jm-boom/server/src/page_materializer.rs:145)；[E:/ppxb/jm-boom/server/src/page_materializer.rs:188](E:/ppxb/jm-boom/server/src/page_materializer.rs:188)；[E:/ppxb/jm-boom/server/src/page_materializer.rs:269](E:/ppxb/jm-boom/server/src/page_materializer.rs:269)；[E:/ppxb/jm-boom/server/src/page_materializer.rs:702](E:/ppxb/jm-boom/server/src/page_materializer.rs:702)；[E:/ppxb/jm-boom/server/src/page_materializer.rs:747](E:/ppxb/jm-boom/server/src/page_materializer.rs:747)；[E:/ppxb/jm-boom/server/src/download/manager.rs:781](E:/ppxb/jm-boom/server/src/download/manager.rs:781)。

- [x] **04 · P2 · 前端显示“取消已暂停任务”，后端却拒绝该转换（已修复）**

  已修复：`stop_task` 仅在目标状态为 Cancelled 时额外允许 Paused，补齐“已暂停 → 已取消”转换，与前端现有取消按钮保持一致。重复暂停及对 Completed、Failed、Cancelled 任务执行暂停/取消仍返回 InvalidState；继续使用现有任务锁、取消通知和持久化流程。

  验证：新增 2 个回归测试。正向测试在旧代码上复现暂停后取消被拒绝，修复后验证返回列表和 SQLite 状态均为 Cancelled，并保留总页数、已完成页数及 generation。反向测试验证重复暂停和终态任务操作被拒绝，且内存与持久化状态不被改动。

  完整路径：[E:/ppxb/jm-boom/web/src/features/downloads/download-task-card.tsx:125](E:/ppxb/jm-boom/web/src/features/downloads/download-task-card.tsx:125)；[E:/ppxb/jm-boom/server/src/download/manager.rs:272](E:/ppxb/jm-boom/server/src/download/manager.rs:272)；[E:/ppxb/jm-boom/server/src/download/manager.rs:423](E:/ppxb/jm-boom/server/src/download/manager.rs:423)；[E:/ppxb/jm-boom/server/src/download/manager.rs:454](E:/ppxb/jm-boom/server/src/download/manager.rs:454)。

- [x] **05 · P2 · 阅读进度保存失败后丢失待保存记录（已修复）**

  已修复：待保存进度只在对应请求成功后清除；普通保存请求串行执行，期间仅保留最新进度。可重试错误按 5、10、20、30 秒退避，之后间隔上限为 30 秒；不可重试的 API 错误不自动循环重试。重试沿用阅读事件的时间戳，并用请求/记录身份避免旧响应清除新进度。pagehide 与卸载使用 keepalive 补写，重复补写去重；卸载后停止后台重试，不增加浏览器持久化历史。

  验证：与第 06 项共新增 10 个实际 React 挂载测试，覆盖失败后不翻页也重试、退避上限、永久错误、连续翻页合并、旧请求成功/失败、keepalive、卸载、跨章节响应和 Strict Mode。测试环境通过开发依赖 happy-dom 提供，相关测试已通过，最新测试统计见文末。

  完整路径：[E:/ppxb/jm-boom/web/src/features/reader/use-reader-history-sync.ts:72](E:/ppxb/jm-boom/web/src/features/reader/use-reader-history-sync.ts:72)；[E:/ppxb/jm-boom/web/src/features/reader/use-reader-history-sync.ts:122](E:/ppxb/jm-boom/web/src/features/reader/use-reader-history-sync.ts:122)；[E:/ppxb/jm-boom/web/src/features/reader/use-reader-history-sync.test.tsx:138](E:/ppxb/jm-boom/web/src/features/reader/use-reader-history-sync.test.tsx:138)。

- [x] **06 · P2 · 阅读历史在服务端提交前失效，返回书架时可能读到旧记录（已修复）**

  已修复：等待 `upsertReadingHistory` 成功后再更新漫画状态缓存并使历史列表失效，避免在 PUT 提交前启动列表刷新。更新状态缓存时比较 lastReadAt，较旧章节请求的迟到响应不会覆盖已确认的新进度。

  验证：实际 QueryClient 测试确认请求未成功时历史缓存不失效、漫画进度不提前更新；成功后才更新和失效，并覆盖旧章节响应晚于新章节完成的情况。

  完整路径：[E:/ppxb/jm-boom/web/src/features/reader/use-reader-history-sync.ts:90](E:/ppxb/jm-boom/web/src/features/reader/use-reader-history-sync.ts:90)；[E:/ppxb/jm-boom/web/src/features/reader/use-reader-history-sync.ts:101](E:/ppxb/jm-boom/web/src/features/reader/use-reader-history-sync.ts:101)；[E:/ppxb/jm-boom/web/src/features/reader/use-reader-history-sync.test.tsx:240](E:/ppxb/jm-boom/web/src/features/reader/use-reader-history-sync.test.tsx:240)；[E:/ppxb/jm-boom/web/src/features/reader/use-reader-history-sync.test.tsx:250](E:/ppxb/jm-boom/web/src/features/reader/use-reader-history-sync.test.tsx:250)。

- [x] **07 · P2 · 共享收藏列表沿用静态内容的缓存策略，跨设备变更长期不可见（已修复）**

  已修复：收藏查询单独设置 `staleTime: 10_000`、`refetchOnMount: true` 和 `refetchOnWindowFocus: true`。缓存过期后，重新进入收藏页或切回窗口会重新查询；本地收藏操作主动使查询失效后，返回页面也会刷新。未过期缓存继续复用，过期时间本身不会触发轮询。

  验证：新增 4 个实际页面挂载回归测试，覆盖过期缓存进入页面、未挂载列表被收藏 mutation 失效后返回、过期列表重新聚焦，以及未过期缓存复用。旧代码在 3 个刷新场景中未发出请求，修复后 4 个测试全部通过。测试文件使用 TanStack Router 的忽略前缀，不会生成额外路由。

  完整路径：[E:/ppxb/jm-boom/web/src/routes/_app/favorites.tsx:26](E:/ppxb/jm-boom/web/src/routes/_app/favorites.tsx:26)；[E:/ppxb/jm-boom/web/src/components/providers.tsx:11](E:/ppxb/jm-boom/web/src/components/providers.tsx:11)；[E:/ppxb/jm-boom/web/src/routes/_app/bookshelf.tsx:33](E:/ppxb/jm-boom/web/src/routes/_app/bookshelf.tsx:33)；[E:/ppxb/jm-boom/web/src/routes/_app/-favorites.test.tsx:169](E:/ppxb/jm-boom/web/src/routes/_app/-favorites.test.tsx:169)。

- [x] **08 · P2 · 收藏总页数减少后没有修正当前页（已修复）**

  已修复：收藏页根据已有有效响应计算至少为 1 的总页数；当前页越界时，使最后有效页的缓存失效并切换到该页，确保取得最新列表和总数。列表清空时保持第 1 页，不会请求第 0 页；响应尚未返回时不使用缺失总数重置页码，首次请求失败后仍可重试原页。分页器继续在不超过一页时隐藏。

  验证：新增 5 个实际页面回归测试，覆盖 21 条减至 20 条、81 条减至 41 条、远端清空、延迟响应和请求失败后重试。3 个缩页场景在旧代码上失败，修复后全部通过，并验证目标页已有新鲜缓存时仍重新请求、显示正确漫画及分页器状态。

  完整路径：[E:/ppxb/jm-boom/web/src/routes/_app/favorites.tsx:49](E:/ppxb/jm-boom/web/src/routes/_app/favorites.tsx:49)；[E:/ppxb/jm-boom/web/src/routes/_app/favorites.tsx:51](E:/ppxb/jm-boom/web/src/routes/_app/favorites.tsx:51)；[E:/ppxb/jm-boom/web/src/routes/_app/-favorites.test.tsx:212](E:/ppxb/jm-boom/web/src/routes/_app/-favorites.test.tsx:212)；[E:/ppxb/jm-boom/web/src/routes/_app/-favorites.test.tsx:248](E:/ppxb/jm-boom/web/src/routes/_app/-favorites.test.tsx:248)。

- [x] **09 · P2 · 详情状态查询失败被当成“未收藏、无阅读历史”（已修复）**

  已修复：详情页分别展示状态加载提示和失败重试入口。没有有效状态时，收藏状态保持 undefined、主阅读目标保持 null，桌面与移动端均显示未知状态并禁用相关操作；收藏回调也拒绝无状态提交。漫画资料与章节列表继续可用，未知阅读进度时不预热默认章节。有缓存的后台刷新失败则保留已有收藏状态和继续阅读位置，并提示正在显示上次记录。

  验证：新增 6 个实际 React 页面/Hook 回归测试，覆盖首次加载、首次失败、重试成功后恢复收藏与第 20 页阅读位置、后台失败保留缓存、确认未收藏后的添加操作，以及直接调用无状态收藏回调不发出写请求。测试同时检查移动端与桌面操作，并确认未知状态不请求默认章节 manifest。

  完整路径：[E:/ppxb/jm-boom/web/src/features/comic-detail/page.tsx:34](E:/ppxb/jm-boom/web/src/features/comic-detail/page.tsx:34)；[E:/ppxb/jm-boom/web/src/features/comic-detail/page.tsx:64](E:/ppxb/jm-boom/web/src/features/comic-detail/page.tsx:64)；[E:/ppxb/jm-boom/web/src/features/comic-detail/use-comic-favorite.ts:17](E:/ppxb/jm-boom/web/src/features/comic-detail/use-comic-favorite.ts:17)；[E:/ppxb/jm-boom/web/src/features/comic-detail/use-comic-favorite.ts:48](E:/ppxb/jm-boom/web/src/features/comic-detail/use-comic-favorite.ts:48)；[E:/ppxb/jm-boom/web/src/features/comic-detail/hero.tsx:107](E:/ppxb/jm-boom/web/src/features/comic-detail/hero.tsx:107)；[E:/ppxb/jm-boom/web/src/features/comic-detail/floating-actions.tsx:24](E:/ppxb/jm-boom/web/src/features/comic-detail/floating-actions.tsx:24)；[E:/ppxb/jm-boom/web/src/features/comic-detail/use-comic-reader-preload.ts:12](E:/ppxb/jm-boom/web/src/features/comic-detail/use-comic-reader-preload.ts:12)；[E:/ppxb/jm-boom/web/src/features/comic-detail/page.test.tsx:202](E:/ppxb/jm-boom/web/src/features/comic-detail/page.test.tsx:202)。

- [x] **10 · P2 · 账号表单在后台状态更新时覆盖用户未保存输入（已修复）**

  已修复：表单只在首次有效状态载入、已保存账号身份变化（含退出账号）或保存成功时重置。后台登录/签到状态与窗口聚焦刷新继续更新状态显示，保留用户名、密码和两个开关的草稿。保存成功通过 mutation 回调明确重置，即使返回状态与缓存相同，也会清空密码框；保存或退出失败保留输入。

  验证：新增 7 个实际设置页挂载回归测试，均在 Strict Mode 下执行，覆盖首次加载、登录/签到状态刷新、窗口聚焦、同状态保存成功、保存失败及重试、账号身份切换和退出成败。旧代码在其中 4 个场景失败，修复后全部通过；前端全量 42 个测试通过。

  完整路径：[E:/ppxb/jm-boom/web/src/features/settings/account-section.tsx:41](E:/ppxb/jm-boom/web/src/features/settings/account-section.tsx:41)；[E:/ppxb/jm-boom/web/src/features/settings/account-section.tsx:47](E:/ppxb/jm-boom/web/src/features/settings/account-section.tsx:47)；[E:/ppxb/jm-boom/web/src/features/settings/page.tsx:79](E:/ppxb/jm-boom/web/src/features/settings/page.tsx:79)；[E:/ppxb/jm-boom/web/src/features/settings/account-section.test.tsx:230](E:/ppxb/jm-boom/web/src/features/settings/account-section.test.tsx:230)。

- [x] **11 · P2 · 手动端点显示的延迟与可用状态可能属于另一个端点（已修复）**

  已修复：选择框按所选端点查找自身的探测结果，缺少结果时显示“未测试”。手动选择与实际使用端点不同时，在选择框下方独立显示“当前使用”接口，支持长地址换行；恢复一致时隐藏提示。保留手动选择、自动模式、测速操作和后端故障切换逻辑。

  验证：新增 6 个实际 React 组件挂载回归测试，覆盖选中端点不可用、两个端点延迟不同、缺少探测结果、恢复选中端点、自动模式以及加载/测速操作。旧代码在其中 4 个场景失败，修复后全部通过；前端全量 58 个测试通过。

  完整路径：[E:/ppxb/jm-boom/web/src/features/settings/api-endpoint-section.tsx:44](E:/ppxb/jm-boom/web/src/features/settings/api-endpoint-section.tsx:44)；[E:/ppxb/jm-boom/web/src/features/settings/api-endpoint-section.tsx:85](E:/ppxb/jm-boom/web/src/features/settings/api-endpoint-section.tsx:85)；[E:/ppxb/jm-boom/web/src/features/settings/api-endpoint-section.test.tsx:88](E:/ppxb/jm-boom/web/src/features/settings/api-endpoint-section.test.tsx:88)。

- [x] **12 · P2 · 下一章预加载依赖不会执行的失败 catch（已修复）**

  已修复：改用 `fetchQuery` 感知失败，成功后才将 manifest 的前 6 页交给现有图片预加载调度器。移除提前写入的已预加载标记，每轮仅对可重试的 API 错误在 5 秒、10 秒后各重试一次，无需继续翻页触发；永久错误或重试耗尽后停止。退出预加载条件、切章或卸载会清理重试定时器，重新满足条件可发起新一轮。保留共享查询与缓存，旧响应不再触发图片预加载，不取消其他读者共享的请求。

  验证：新增 10 个实际 React Hook 挂载测试，覆盖失败后自动重试、重试间隔和上限、永久错误、重新进入预加载条件、切章、迟到响应、共享请求、卸载及 Strict Mode，并确认新鲜缓存与并发请求复用。旧实现已复现失败被当作成功且不重试；修复后前端全量 52 个测试通过。

  完整路径：[E:/ppxb/jm-boom/web/src/features/reader/use-next-chapter-prefetch.ts:43](E:/ppxb/jm-boom/web/src/features/reader/use-next-chapter-prefetch.ts:43)；[E:/ppxb/jm-boom/web/src/features/reader/use-next-chapter-prefetch.ts:61](E:/ppxb/jm-boom/web/src/features/reader/use-next-chapter-prefetch.ts:61)；[E:/ppxb/jm-boom/web/src/features/reader/use-next-chapter-prefetch.integration.test.tsx:162](E:/ppxb/jm-boom/web/src/features/reader/use-next-chapter-prefetch.integration.test.tsx:162)。

- [ ] **13 · 待确认 · 每周推荐的上游分页行为**

  原判断仅依据客户端与服务端接受 `page` 参数，不能据此确认上游实际分页或当前存在内容遗漏。2026-10-22 通过内置上游端点匿名核对期数 262、261、260、197、132、1 的三种类型，共 18 组数据，第一页返回条数均等于 `total`（0 至 20 条）。其中 10 组比较第 1、2 页的漫画 ID 列表，结果完全相同；期数 262 的 manga 类型第 3 页也相同。

  当前样本已返回总数所指的完整集合，未发现需要翻页的数据，因此暂不添加分页器。该结果不证明所有期数或未来接口都不分页；若出现 `total > items.length` 的实际样本，应先核实不同页返回不同内容及每页容量，再补齐页码、查询缓存键和分页器，避免产生重复页或错误的末页判断。

  完整路径：[E:/ppxb/jm-boom/web/src/routes/_app/explore/weekly.tsx:81](E:/ppxb/jm-boom/web/src/routes/_app/explore/weekly.tsx:81)；[E:/ppxb/jm-boom/web/src/lib/api/home.ts:103](E:/ppxb/jm-boom/web/src/lib/api/home.ts:103)；[E:/ppxb/jm-boom/web/src/lib/query-keys.ts:24](E:/ppxb/jm-boom/web/src/lib/query-keys.ts:24)；[E:/ppxb/jm-boom/server/src/application/comic/home/mod.rs:178](E:/ppxb/jm-boom/server/src/application/comic/home/mod.rs:178)。

- [x] **14 · P2 · 每周筛选成功返回空数据时会永久显示骨架屏（已修复）**

  已修复：根据筛选查询的 pending/success 状态区分加载和空结果。期数或类型为空时显示“暂无每周推荐”，同时结束筛选框及漫画区的占位动画；漫画请求必须在两组筛选项均非空后启用，地址栏残留 ID 不会绕过这一条件。筛选有效时保留列表加载、空漫画列表及两类请求各自的失败重试。

  验证：新增 8 个实际页面挂载回归测试，使用真实 Router 和 QueryClient，覆盖带筛选 ID 的首次加载、两组筛选均空、仅期数为空、仅类型为空、筛选失败重试、漫画失败重试、空漫画列表及后台刷新后筛选变空。旧代码已复现永久骨架屏和筛选尚未返回时提前请求漫画；修复后前端全量 66 个测试通过。测试文件使用 Router 忽略前缀，不生成额外路由。

  完整路径：[E:/ppxb/jm-boom/web/src/routes/_app/explore/weekly.tsx:79](E:/ppxb/jm-boom/web/src/routes/_app/explore/weekly.tsx:79)；[E:/ppxb/jm-boom/web/src/routes/_app/explore/weekly.tsx:133](E:/ppxb/jm-boom/web/src/routes/_app/explore/weekly.tsx:133)；[E:/ppxb/jm-boom/web/src/routes/_app/explore/-weekly.test.tsx:169](E:/ppxb/jm-boom/web/src/routes/_app/explore/-weekly.test.tsx:169)。

- [ ] **15 · P2 · 评论分页仅由滚动事件触发，短列表无法继续加载**

  当首批评论高度不足以产生滚动条且 `hasNextPage` 为 true 时，用户无法触发 `handleScroll`，底部只有“继续向下滚动”文案。应使用末尾可见性检测补齐容器，或提供显式加载更多按钮。

  完整路径：[E:/ppxb/jm-boom/web/src/features/comic-detail/comments.tsx:62](E:/ppxb/jm-boom/web/src/features/comic-detail/comments.tsx:62)；[E:/ppxb/jm-boom/web/src/features/comic-detail/comments.tsx:146](E:/ppxb/jm-boom/web/src/features/comic-detail/comments.tsx:146)。

- [ ] **16 · P2 · 下载 mutation 的缓存结果可能被较早的轮询响应覆盖**

  任务操作成功后直接 `setQueryData`，没有协调已进行中的列表请求。先开始的轮询若晚于删除、暂停响应返回，可覆盖新快照；删除后的无活动任务还可能停止后续轮询。应取消或隔离旧查询，并在操作结束后校准列表，保留现有轮询方案。

  完整路径：[E:/ppxb/jm-boom/web/src/features/downloads/use-download-tasks.ts:63](E:/ppxb/jm-boom/web/src/features/downloads/use-download-tasks.ts:63)；[E:/ppxb/jm-boom/web/src/features/comic-detail/use-comic-download.ts:29](E:/ppxb/jm-boom/web/src/features/comic-detail/use-comic-download.ts:29)；[E:/ppxb/jm-boom/web/src/lib/api/client.ts:118](E:/ppxb/jm-boom/web/src/lib/api/client.ts:118)。

- [ ] **17 · P2 · 无需解扰的损坏图片可能被持久缓存，重试仍返回同一坏数据**

  `prepare_page_image` 对直通格式只识别文件头就返回，阅读缓存读写没有使用下载存储已有的完整性校验。上游若以成功响应返回截断图片，浏览器重试参数不会改变服务端缓存键。应在共享写入路径进行轻量完整性检查或使坏缓存可失效，保持原样缓存且不额外完整解码。

  完整路径：[E:/ppxb/jm-boom/server/src/reader/page.rs:85](E:/ppxb/jm-boom/server/src/reader/page.rs:85)；[E:/ppxb/jm-boom/server/src/cache/mod.rs:173](E:/ppxb/jm-boom/server/src/cache/mod.rs:173)；[E:/ppxb/jm-boom/server/src/download/storage.rs:32](E:/ppxb/jm-boom/server/src/download/storage.rs:32)；[E:/ppxb/jm-boom/web/src/features/reader/reader-image.tsx:282](E:/ppxb/jm-boom/web/src/features/reader/reader-image.tsx:282)。

- [ ] **18 · P2 · 登录后的跳转校验允许反斜杠形式的外站跳转**

  `safeRedirect` 只排除 `//`；形如 `/\example.invalid` 的值仍通过，而浏览器 URL 规则会把它解析为 `https://example.invalid/`。已用 URL 解析器复现。应解析目标 URL 后验证 origin 与当前站点一致，再执行跳转。

  完整路径：[E:/ppxb/jm-boom/web/src/routes/login.tsx:104](E:/ppxb/jm-boom/web/src/routes/login.tsx:104)。

- [ ] **19 · P2 · 账号密码加密使用公开的固定密钥，不能保护数据库中的凭据**

  `credential_key` 只从源码常量计算，所有实例共用同一密钥；取得 SQLite 密文即可按公开算法还原密码。这是本地凭据存储的问题，与上游协议要求的固定加密种子不同。应使用实例独立且与数据库分离保存的密钥，并采用带认证的加密方式。

  完整路径：[E:/ppxb/jm-boom/server/src/application/account.rs:16](E:/ppxb/jm-boom/server/src/application/account.rs:16)；[E:/ppxb/jm-boom/server/src/application/account.rs:391](E:/ppxb/jm-boom/server/src/application/account.rs:391)。

- [ ] **20 · P3 · 账号功能与“匿名上游请求”的架构约定存在冲突**

  当前 AGENTS 约定匿名 JM API 请求，但同一个 JmClient 在账号登录后对通用 GET 自动附加 JWT；章节和 setting 的独立请求路径又不附加。应明确认证是否仅服务账号/签到，按最终约定划分调用边界并同步项目说明，避免为去重而扩大认证作用范围。

  完整路径：[E:/ppxb/jm-boom/AGENTS.MD:7](E:/ppxb/jm-boom/AGENTS.MD:7)；[E:/ppxb/jm-boom/server/src/jm/client.rs:51](E:/ppxb/jm-boom/server/src/jm/client.rs:51)；[E:/ppxb/jm-boom/server/src/jm/chapter.rs:78](E:/ppxb/jm-boom/server/src/jm/chapter.rs:78)；[E:/ppxb/jm-boom/server/src/bootstrap/state.rs:46](E:/ppxb/jm-boom/server/src/bootstrap/state.rs:46)。

## 网络、缓存与运行成本

- [ ] **21 · P2 · 下载批量删除实际是 N 次串行请求并接收 N 次完整列表**

  `removeDownloadTasks` 逐个 await 单删 API，只消费最后一次结果；部分成功后若某次失败，前端还不会应用已完成删除的结果。应新增批量删除接口，一次提交 ID，由服务端完成 worker 停止及 manifest/目录清理，返回一次最终结果。

  完整路径：[E:/ppxb/jm-boom/web/src/features/downloads/use-download-tasks.ts:80](E:/ppxb/jm-boom/web/src/features/downloads/use-download-tasks.ts:80)；[E:/ppxb/jm-boom/web/src/lib/api/download.ts:75](E:/ppxb/jm-boom/web/src/lib/api/download.ts:75)；[E:/ppxb/jm-boom/server/src/api/downloads.rs:48](E:/ppxb/jm-boom/server/src/api/downloads.rs:48)；[E:/ppxb/jm-boom/server/src/download/manager.rs:228](E:/ppxb/jm-boom/server/src/download/manager.rs:228)。

- [ ] **22 · P2 · 上游大页被切成小页后，每次翻页重复下载同一份源数据**

  `weekly_section` 把上游 40 条拆为 20 条，latest/ranking 把 80 条拆为 20 条，但每个前端页都会重新请求对应上游页。因此依次查看 4 个 ranking 页会对同一源页发起 4 次请求。应在现有边界内缓存源页或复用源页结果，按端点、筛选与必要的账号上下文区分缓存。

  完整路径：[E:/ppxb/jm-boom/server/src/application/comic/home/section.rs:65](E:/ppxb/jm-boom/server/src/application/comic/home/section.rs:65)；[E:/ppxb/jm-boom/server/src/application/comic/home/section.rs:110](E:/ppxb/jm-boom/server/src/application/comic/home/section.rs:110)；[E:/ppxb/jm-boom/server/src/application/comic/home/section.rs:136](E:/ppxb/jm-boom/server/src/application/comic/home/section.rs:136)；[E:/ppxb/jm-boom/server/src/application/comic/mod.rs:50](E:/ppxb/jm-boom/server/src/application/comic/mod.rs:50)。

- [ ] **23 · P2 · 评论接口为前端不展示的头像串行等待图片域名查询**

  评论列表拿到后仍执行 `self.image_host().await`，只用于拼接 avatar；CommentItem 和 ReplyItem 均不渲染头像。图片域名缓存失效或上游故障时，正文评论也被额外阻塞。应删除这一不被消费的依赖，或在确需头像的消费路径单独处理。

  完整路径：[E:/ppxb/jm-boom/server/src/application/comic/comments.rs:38](E:/ppxb/jm-boom/server/src/application/comic/comments.rs:38)；[E:/ppxb/jm-boom/server/src/application/comic/comments.rs:99](E:/ppxb/jm-boom/server/src/application/comic/comments.rs:99)；[E:/ppxb/jm-boom/web/src/features/comic-detail/comments.tsx:141](E:/ppxb/jm-boom/web/src/features/comic-detail/comments.tsx:141)。

- [ ] **24 · P2 · 普通查询没有向 fetch 传递 TanStack Query 的取消信号**

  `apiClient.get` 不接受 RequestInit/AbortSignal，搜索、列表和 manifest 的 queryFn 也未消费 signal。快速切换关键词、筛选或章节后，失去消费者的旧请求仍继续占用连接和上游资源。应贯通查询取消信号，保留确有用途的预取请求和历史 keepalive 写入。

  完整路径：[E:/ppxb/jm-boom/web/src/lib/api/client.ts:118](E:/ppxb/jm-boom/web/src/lib/api/client.ts:118)；[E:/ppxb/jm-boom/web/src/routes/_app/explore/search.tsx:62](E:/ppxb/jm-boom/web/src/routes/_app/explore/search.tsx:62)；[E:/ppxb/jm-boom/web/src/features/reader/use-reader-manifest-query.ts:9](E:/ppxb/jm-boom/web/src/features/reader/use-reader-manifest-query.ts:9)；[E:/ppxb/jm-boom/web/src/lib/api/reader.ts:15](E:/ppxb/jm-boom/web/src/lib/api/reader.ts:15)。

- [ ] **25 · P2 · 服务端预热只限制执行并发，没有限制排队任务数量**

  `prewarm_pages` 先为每页 `tokio::spawn`，再在任务内部等待两个许可。连续翻页或多设备阅读时，过时且重复的预热任务可持续堆积；single-flight 仅合并物化工作，并不消除这些待执行任务。应给预热队列加容量及去重/丢弃过期任务规则，继续复用页面物化服务。

  完整路径：[E:/ppxb/jm-boom/server/src/api/reader.rs:166](E:/ppxb/jm-boom/server/src/api/reader.rs:166)；[E:/ppxb/jm-boom/server/src/page_materializer.rs:116](E:/ppxb/jm-boom/server/src/page_materializer.rs:116)。

- [ ] **26 · P2 · 缓存淘汰在全局写锁内逐条删文件和提交 SQL**

  容量越界时，LRU 淘汰一直持有 `operation_lock.write()`，逐条执行文件删除及独立 SQL，直到容量降到一半；期间所有缓存读写均等待。默认容量 5 GiB，集中淘汰时可能形成明显停顿。应批量提交索引删除并控制单次工作量，再用接近容量上限的缓存验证阻塞时间。

  完整路径：[E:/ppxb/jm-boom/server/src/cache/maintenance.rs:68](E:/ppxb/jm-boom/server/src/cache/maintenance.rs:68)；[E:/ppxb/jm-boom/server/src/cache/maintenance.rs:92](E:/ppxb/jm-boom/server/src/cache/maintenance.rs:92)；[E:/ppxb/jm-boom/server/src/cache/repository.rs:133](E:/ppxb/jm-boom/server/src/cache/repository.rs:133)；[E:/ppxb/jm-boom/server/src/cache/mod.rs:238](E:/ppxb/jm-boom/server/src/cache/mod.rs:238)。

- [ ] **27 · P2 · 阅读图片尺寸缓存没有容量或会话清理边界**

  `loadedImageSizes` 是模块级 Map，每张看过的图片都会永久新增 URL 与尺寸记录，路由卸载不会清理。它保存的是尺寸而非图片二进制，但长时间阅读不同漫画仍会线性增长。应设置合理容量或按章节淘汰，同时保留当前章节回滚时避免布局跳动的用途。

  完整路径：[E:/ppxb/jm-boom/web/src/features/reader/reader-image.tsx:15](E:/ppxb/jm-boom/web/src/features/reader/reader-image.tsx:15)；[E:/ppxb/jm-boom/web/src/features/reader/reader-image.tsx:270](E:/ppxb/jm-boom/web/src/features/reader/reader-image.tsx:270)。

## 原生控件、shadcn 使用与 UI 一致性

- [ ] **28 · P3 · 自动阅读设置重新实现原生 range 滑块的浏览器皮肤**

  `ReaderRangeSetting` 使用 `<input type="range">`，自行维护 Firefox/WebKit 的轨道与滑块伪元素；阅读进度已有 shadcn Slider，形成两套外观和交互维护路径。应复用现有 Slider 的尺寸、轨道和 thumb 扩展能力，保留标签及数值单位。

  完整路径：[E:/ppxb/jm-boom/web/src/features/reader/reader-settings-menu.tsx:171](E:/ppxb/jm-boom/web/src/features/reader/reader-settings-menu.tsx:171)；[E:/ppxb/jm-boom/web/src/components/ui/slider.tsx:6](E:/ppxb/jm-boom/web/src/components/ui/slider.tsx:6)；[E:/ppxb/jm-boom/web/src/features/reader/reader-progress-slider.tsx:57](E:/ppxb/jm-boom/web/src/features/reader/reader-progress-slider.tsx:57)。

- [ ] **29 · P2 · 把普通表单控件放入 DropdownMenu，破坏菜单键盘导航**

  阅读设置在 menu 内混放原生方向按钮、Switch 和 range，它们未注册为菜单项；Radix Menu 又会拦截内部 Tab 键，因此键盘用户难以遍历这些设置。应改为适合表单的 Popover/面板，或把离散选项改为正规的 RadioItem/CheckboxItem；方向选中状态也应通过语义属性表达。

  完整路径：[E:/ppxb/jm-boom/web/src/features/reader/reader-settings-menu.tsx:72](E:/ppxb/jm-boom/web/src/features/reader/reader-settings-menu.tsx:72)；[E:/ppxb/jm-boom/web/src/features/reader/reader-settings-menu.tsx:218](E:/ppxb/jm-boom/web/src/features/reader/reader-settings-menu.tsx:218)；[E:/ppxb/jm-boom/web/node_modules/@radix-ui/react-menu/dist/index.mjs:307](E:/ppxb/jm-boom/web/node_modules/@radix-ui/react-menu/dist/index.mjs:307)。

- [ ] **30 · P2 · 多处设置和图标控件缺少可访问名称或标签关联**

  SettingRow 的标题只是 div，没有绑定 Switch；账号输入主要靠 placeholder，登录密码没有 label；主题图标按钮、漫画选择 Checkbox 和隐藏 SelectValue 的搜索排序按钮也缺少明确名称。应使用 Label/htmlFor 或 aria-label/aria-labelledby，复用 SettingRow 时同时解决关联能力。

  完整路径：[E:/ppxb/jm-boom/web/src/features/settings/shared.tsx:31](E:/ppxb/jm-boom/web/src/features/settings/shared.tsx:31)；[E:/ppxb/jm-boom/web/src/features/settings/account-section.tsx:58](E:/ppxb/jm-boom/web/src/features/settings/account-section.tsx:58)；[E:/ppxb/jm-boom/web/src/features/settings/privacy-section.tsx:16](E:/ppxb/jm-boom/web/src/features/settings/privacy-section.tsx:16)；[E:/ppxb/jm-boom/web/src/features/settings/appearance-section.tsx:24](E:/ppxb/jm-boom/web/src/features/settings/appearance-section.tsx:24)；[E:/ppxb/jm-boom/web/src/features/reader/reader-settings-menu.tsx:115](E:/ppxb/jm-boom/web/src/features/reader/reader-settings-menu.tsx:115)；[E:/ppxb/jm-boom/web/src/routes/login.tsx:80](E:/ppxb/jm-boom/web/src/routes/login.tsx:80)；[E:/ppxb/jm-boom/web/src/routes/_app/explore/search.tsx:140](E:/ppxb/jm-boom/web/src/routes/_app/explore/search.tsx:140)；[E:/ppxb/jm-boom/web/src/components/comic/comic-card.tsx:72](E:/ppxb/jm-boom/web/src/components/comic/comic-card.tsx:72)。

- [ ] **31 · P2 · 封面组件声明 title 却未使用，img 也没有 alt**

  `ComicCoverProps.title` 在调用端反复传入，但组件未解构它，图片缺少 alt。应根据封面是否承担独立信息选择有意义的 alt 或明确的 `alt=""`；若确定为装饰图，同步删除无用 title prop，避免保留无效接口。

  完整路径：[E:/ppxb/jm-boom/web/src/components/comic/comic-cover.tsx:10](E:/ppxb/jm-boom/web/src/components/comic/comic-cover.tsx:10)；[E:/ppxb/jm-boom/web/src/components/comic/comic-cover.tsx:26](E:/ppxb/jm-boom/web/src/components/comic/comic-cover.tsx:26)；[E:/ppxb/jm-boom/web/src/components/comic/comic-cover.tsx:51](E:/ppxb/jm-boom/web/src/components/comic/comic-cover.tsx:51)。

- [ ] **32 · P2 · 阅读器隐藏的顶部栏仍可被键盘聚焦和读屏访问**

  顶部栏隐藏时只使用 opacity、位移和 `pointer-events-none`，返回与重载按钮仍在 Tab 顺序和可访问树中。应在隐藏状态使用 inert 或同步处理可访问性与焦点，保留显隐动画。

  完整路径：[E:/ppxb/jm-boom/web/src/features/reader/reader-bars.tsx:28](E:/ppxb/jm-boom/web/src/features/reader/reader-bars.tsx:28)。

- [ ] **33 · P2 · 分页禁用只限制鼠标，末页仍可通过键盘触发下一页**

  ListPagination 使用 `href="#"` 和 `pointer-events-none`；`changePage` 没有校验 `hasMore`。最小复现中 `page=3, hasMore=false` 的下一页处理仍调用 `onPageChange(4)`。章节分页也只靠样式禁用。应让事件守卫与禁用状态一致，并为实际操作使用有正确禁用行为的按钮或完整链接语义。

  完整路径：[E:/ppxb/jm-boom/web/src/components/list-pagination.tsx:22](E:/ppxb/jm-boom/web/src/components/list-pagination.tsx:22)；[E:/ppxb/jm-boom/web/src/components/list-pagination.tsx:54](E:/ppxb/jm-boom/web/src/components/list-pagination.tsx:54)；[E:/ppxb/jm-boom/web/src/features/comic-detail/chapters.tsx:178](E:/ppxb/jm-boom/web/src/features/comic-detail/chapters.tsx:178)；[E:/ppxb/jm-boom/web/src/components/ui/pagination.tsx:38](E:/ppxb/jm-boom/web/src/components/ui/pagination.tsx:38)。

- [ ] **34 · P2 · 阅读进度滑块覆盖了 shadcn 的可见焦点样式**

  业务端给 thumb 设置 `ring-0 hover:ring-0 focus-visible:ring-0`，基础 Slider 同时隐藏默认 outline，导致键盘聚焦时没有清晰提示。应保留专门的 focus-visible 样式；需要缩小 thumb 时不要一并删除焦点反馈。

  完整路径：[E:/ppxb/jm-boom/web/src/features/reader/reader-progress-slider.tsx:65](E:/ppxb/jm-boom/web/src/features/reader/reader-progress-slider.tsx:65)；[E:/ppxb/jm-boom/web/src/components/ui/slider.tsx:58](E:/ppxb/jm-boom/web/src/components/ui/slider.tsx:58)。

- [ ] **35 · P3 · 底部导航 ul 的直接子元素不是 li**

  `ul` 渲染的 NavItem 最终返回 Tooltip 包裹的 Link，实际列表缺少 li。应补齐语义列表项；无需改变底部胶囊导航布局。

  完整路径：[E:/ppxb/jm-boom/web/src/components/floating-nav.tsx:25](E:/ppxb/jm-boom/web/src/components/floating-nav.tsx:25)；[E:/ppxb/jm-boom/web/src/components/floating-nav.tsx:39](E:/ppxb/jm-boom/web/src/components/floating-nav.tsx:39)。

- [ ] **36 · P3 · 账号与版本 Badge 重复覆盖高度、字体和图标尺寸**

  四处重复 `h-7 min-w-20 px-3 text-sm [&>svg]:size-4!`，并用 important 与基础 Badge 的图标尺寸规则竞争。应把这个已重复使用的尺寸规格收敛为 Badge size 或小型业务组件，避免每处调整。

  完整路径：[E:/ppxb/jm-boom/web/src/features/settings/account-section.tsx:79](E:/ppxb/jm-boom/web/src/features/settings/account-section.tsx:79)；[E:/ppxb/jm-boom/web/src/features/settings/system-sections.tsx:84](E:/ppxb/jm-boom/web/src/features/settings/system-sections.tsx:84)；[E:/ppxb/jm-boom/web/src/components/ui/badge.tsx:7](E:/ppxb/jm-boom/web/src/components/ui/badge.tsx:7)。

- [ ] **37 · P3 · 漫画卡片的 Checkbox 单独硬编码绿色选中样式**

  共享 Checkbox 使用 primary 主题色，漫画卡片却覆盖为 green-500，并重复添加相同 dark 规则，下载章节选择又使用默认样式。同一种选择交互出现两套配色。应回归共享主题色，或在明确需要区分时提供统一的语义变体。

  完整路径：[E:/ppxb/jm-boom/web/src/components/comic/comic-card.tsx:72](E:/ppxb/jm-boom/web/src/components/comic/comic-card.tsx:72)；[E:/ppxb/jm-boom/web/src/components/ui/checkbox.tsx:10](E:/ppxb/jm-boom/web/src/components/ui/checkbox.tsx:10)；[E:/ppxb/jm-boom/web/src/features/comic-detail/download-drawer.tsx:95](E:/ppxb/jm-boom/web/src/features/comic-detail/download-drawer.tsx:95)。

- [ ] **38 · P3 · 阅读器工具按钮在多个组件重复覆盖 shadcn Button 规格**

  `CHAPTER_BUTTON_CLASS` 与 `READER_SETTING_BUTTON_CLASS` 大部分相同，顶部栏又各自写尺寸、圆角和 hover 色，选了 `xs/icon-sm` 后再覆盖高度与字体。应共享稳定的阅读器按钮规格或薄包装，继续保留移动端触控尺寸和独立阅读器布局。

  完整路径：[E:/ppxb/jm-boom/web/src/features/reader/reader-chapter-controls.tsx:12](E:/ppxb/jm-boom/web/src/features/reader/reader-chapter-controls.tsx:12)；[E:/ppxb/jm-boom/web/src/features/reader/reader-settings-menu.tsx:23](E:/ppxb/jm-boom/web/src/features/reader/reader-settings-menu.tsx:23)；[E:/ppxb/jm-boom/web/src/features/reader/reader-bars.tsx:37](E:/ppxb/jm-boom/web/src/features/reader/reader-bars.tsx:37)；[E:/ppxb/jm-boom/web/src/components/ui/button.tsx:24](E:/ppxb/jm-boom/web/src/components/ui/button.tsx:24)。

- [ ] **39 · P3 · 警告与确认弹窗重复手写同一套头部布局**

  NsfwStartupDialog 与 ConfirmDialog 复制了图标容器、标题、说明的结构和 class，并绕过已有 AlertDialogHeader/Media 的布局能力；有无 icon 时标题字号也不同。应复用一个小型弹窗头部或既有 primitives，分别保留两者的确认行为。

  完整路径：[E:/ppxb/jm-boom/web/src/routes/__root.tsx:65](E:/ppxb/jm-boom/web/src/routes/__root.tsx:65)；[E:/ppxb/jm-boom/web/src/components/confirm-dialog.tsx:42](E:/ppxb/jm-boom/web/src/components/confirm-dialog.tsx:42)；[E:/ppxb/jm-boom/web/src/components/ui/alert-dialog.tsx:61](E:/ppxb/jm-boom/web/src/components/ui/alert-dialog.tsx:61)。

- [ ] **40 · P3 · 存在可直接删除的同值响应式、主题和默认样式覆盖**

  例如导航 `bottom-4 sm:bottom-4`，阅读底栏 `bottom-8 sm:bottom-8`、`sm:w-[480px] lg:w-[480px]`，弹窗 `bg-destructive/10 dark:bg-destructive/10`，以及重复设置 AlertDialogDescription 已有的字号/颜色。逐项比对基础组件后删除相同规则；不要把布局所需的 className 一概视为冗余。

  完整路径：[E:/ppxb/jm-boom/web/src/components/floating-nav.tsx:24](E:/ppxb/jm-boom/web/src/components/floating-nav.tsx:24)；[E:/ppxb/jm-boom/web/src/features/reader/reader-bars.tsx:117](E:/ppxb/jm-boom/web/src/features/reader/reader-bars.tsx:117)；[E:/ppxb/jm-boom/web/src/components/confirm-dialog.tsx:45](E:/ppxb/jm-boom/web/src/components/confirm-dialog.tsx:45)；[E:/ppxb/jm-boom/web/src/routes/__root.tsx:66](E:/ppxb/jm-boom/web/src/routes/__root.tsx:66)；[E:/ppxb/jm-boom/web/src/components/ui/alert-dialog.tsx:119](E:/ppxb/jm-boom/web/src/components/ui/alert-dialog.tsx:119)。

## 冗余、过度防御和抽象整理

- [ ] **41 · P3 · 多个查询重复声明 QueryClient 已有的默认配置**

  推荐、排行、分区、搜索、每周列表重复声明完全相同的 staleTime/gcTime/refetch 配置；详情、评论、阅读器也重复 false 的 refetch 选项。删除相同字段，只保留与默认值不同的 TTL、retry、enabled、轮询和刷新设置。

  完整路径：[E:/ppxb/jm-boom/web/src/components/providers.tsx:11](E:/ppxb/jm-boom/web/src/components/providers.tsx:11)；[E:/ppxb/jm-boom/web/src/features/home/page.tsx:14](E:/ppxb/jm-boom/web/src/features/home/page.tsx:14)；[E:/ppxb/jm-boom/web/src/routes/_app/explore/ranking.tsx:40](E:/ppxb/jm-boom/web/src/routes/_app/explore/ranking.tsx:40)；[E:/ppxb/jm-boom/web/src/routes/_app/explore/list.tsx:61](E:/ppxb/jm-boom/web/src/routes/_app/explore/list.tsx:61)；[E:/ppxb/jm-boom/web/src/routes/_app/explore/search.tsx:62](E:/ppxb/jm-boom/web/src/routes/_app/explore/search.tsx:62)；[E:/ppxb/jm-boom/web/src/routes/_app/explore/weekly.tsx:32](E:/ppxb/jm-boom/web/src/routes/_app/explore/weekly.tsx:32)；[E:/ppxb/jm-boom/web/src/features/comic-detail/page.tsx:26](E:/ppxb/jm-boom/web/src/features/comic-detail/page.tsx:26)；[E:/ppxb/jm-boom/web/src/features/comic-detail/comments.tsx:29](E:/ppxb/jm-boom/web/src/features/comic-detail/comments.tsx:29)；[E:/ppxb/jm-boom/web/src/features/reader/use-reader-manifest-query.ts:8](E:/ppxb/jm-boom/web/src/features/reader/use-reader-manifest-query.ts:8)；[E:/ppxb/jm-boom/web/src/features/reader/use-reader-chapter-info.ts:19](E:/ppxb/jm-boom/web/src/features/reader/use-reader-chapter-info.ts:19)；[E:/ppxb/jm-boom/web/src/features/settings/use-settings-data.ts:19](E:/ppxb/jm-boom/web/src/features/settings/use-settings-data.ts:19)。

- [ ] **42 · P3 · 阅读器会话同时依赖路由 key 重建和内部 scope 重置**

  ReaderRoute 已按 comicId/page 给 ReaderPage 设置 key，useReaderSession 又维护 scope、scopeRef、带 scope 的 position 和 scope 变化 effect；当前唯一调用路径下，这套跨 scope 重置逻辑不会被需要。应统一会话重建责任，保留页码夹取和虚拟列表导航命令，不合并其他职责 hook。

  完整路径：[E:/ppxb/jm-boom/web/src/routes/reader/$comicId.tsx:18](E:/ppxb/jm-boom/web/src/routes/reader/$comicId.tsx:18)；[E:/ppxb/jm-boom/web/src/features/reader/use-reader-session.ts:32](E:/ppxb/jm-boom/web/src/features/reader/use-reader-session.ts:32)；[E:/ppxb/jm-boom/web/src/features/reader/use-reader-session.ts:92](E:/ppxb/jm-boom/web/src/features/reader/use-reader-session.ts:92)；[E:/ppxb/jm-boom/web/src/features/reader/reader-page.tsx:63](E:/ppxb/jm-boom/web/src/features/reader/reader-page.tsx:63)。

- [ ] **43 · P3 · 进度滑块保留了只写不读的 ref**

  `previewIndexRef` 初始化后只被赋值，从未读取；真正渲染和提交依赖 previewIndex/currentIndex。应删除该 ref 及相应赋值，减少冗余状态来源。

  完整路径：[E:/ppxb/jm-boom/web/src/features/reader/reader-progress-slider.tsx:19](E:/ppxb/jm-boom/web/src/features/reader/reader-progress-slider.tsx:19)。

- [ ] **44 · P3 · API 层的纯数字 ID 和页码校验重复实现**

  favorites/history/comics/covers 各有近似数字判断，收藏与历史又重复 page > 0 校验；reader 使用 u32 解析且在入口和 materialize_page 中重复验证。应只共享纯校验/解析函数，保留各模块错误类型、范围需求、文案和日志上下文。

  完整路径：[E:/ppxb/jm-boom/server/src/api/favorites.rs:91](E:/ppxb/jm-boom/server/src/api/favorites.rs:91)；[E:/ppxb/jm-boom/server/src/api/history.rs:121](E:/ppxb/jm-boom/server/src/api/history.rs:121)；[E:/ppxb/jm-boom/server/src/api/comics.rs:128](E:/ppxb/jm-boom/server/src/api/comics.rs:128)；[E:/ppxb/jm-boom/server/src/api/covers.rs:22](E:/ppxb/jm-boom/server/src/api/covers.rs:22)；[E:/ppxb/jm-boom/server/src/api/reader.rs:84](E:/ppxb/jm-boom/server/src/api/reader.rs:84)；[E:/ppxb/jm-boom/server/src/api/reader.rs:218](E:/ppxb/jm-boom/server/src/api/reader.rs:218)。

- [ ] **45 · P3 · 响应信封使用了不参与数据结构的伪泛型**

  `ApiResponse<T>` 的 data 实际是 `serde_json::Value`，T 只通过被 serde 跳过的 PhantomData 出现；真正的类型转换发生在 decode_response<T> 内。应让信封成为普通结构体，仅在最终解码函数保留泛型，减少无意义的类型约束。

  完整路径：[E:/ppxb/jm-boom/server/src/jm/client.rs:365](E:/ppxb/jm-boom/server/src/jm/client.rs:365)。

- [ ] **46 · P3 · 存在已无调用的配置和门禁清理接口**

  全仓搜索未发现 `UI.SHOW_COVER_MASK`、前端 `DOWNLOAD.PROGRESS_PERSIST_INTERVAL`、`clearAccessGateGrant` 的调用；当前封面由 hideCovers 控制，下载进度持久化在后端。这些入口会让维护者误以为修改它们能改变行为，应删除或在真实功能需要时再引入。

  完整路径：[E:/ppxb/jm-boom/web/src/lib/constants.ts:52](E:/ppxb/jm-boom/web/src/lib/constants.ts:52)；[E:/ppxb/jm-boom/web/src/lib/access-gate.ts:23](E:/ppxb/jm-boom/web/src/lib/access-gate.ts:23)；[E:/ppxb/jm-boom/server/src/download/state.rs:8](E:/ppxb/jm-boom/server/src/download/state.rs:8)。

- [ ] **47 · P2 · 宽松数字反序列化会把越界值截断为另一个有效 ID**

  `u32_from_any` 和账号模块的 `u32_from_value` 对 JSON u64 直接 `as u32`，例如 4294967297 会得到 1，其他异常又默认为 0。应使用有界转换，保留已知的字符串/数字兼容形式，避免静默改变身份或统计值。

  完整路径：[E:/ppxb/jm-boom/server/src/jm/serde_ext.rs:55](E:/ppxb/jm-boom/server/src/jm/serde_ext.rs:55)；[E:/ppxb/jm-boom/server/src/application/account.rs:409](E:/ppxb/jm-boom/server/src/application/account.rs:409)。

- [ ] **48 · P2 · 章节解析把所有信封错误都吞掉并尝试无关兜底**

  `fetch_chapter` 对 decrypt_chapter_payload 使用 `if let Ok`，上游业务错误、缺 data、解密失败均丢失原始原因，然后当成裸 JSON images 解析。普通错误信封可能最终只剩 MissingData。应按响应形态决定兼容分支，识别为信封后保留其真实错误。

  完整路径：[E:/ppxb/jm-boom/server/src/jm/chapter.rs:102](E:/ppxb/jm-boom/server/src/jm/chapter.rs:102)；[E:/ppxb/jm-boom/server/src/jm/chapter.rs:134](E:/ppxb/jm-boom/server/src/jm/chapter.rs:134)。

- [ ] **49 · P3 · 阅读历史单条 UPSERT 外层保留了不必要的显式事务**

  `upsert` 的事务里只执行一次 INSERT ... ON CONFLICT，没有配套写操作；SQLite 单条语句自身已具备原子性。应直接执行该 UPSERT 并删除只为传递 transaction 的辅助层，保留时间戳防旧覆盖条件。

  完整路径：[E:/ppxb/jm-boom/server/src/application/history.rs:150](E:/ppxb/jm-boom/server/src/application/history.rs:150)；[E:/ppxb/jm-boom/server/src/application/history.rs:186](E:/ppxb/jm-boom/server/src/application/history.rs:186)。

- [ ] **50 · P3 · 探索列表重复 loading/error/empty/grid/pagination 拼装**

  排行、分区列表和搜索各自复制相同的错误+重试、骨架、空结果和网格结构，下载页的错误状态则变成没有重试入口的红字 Card。应先复用小型错误/加载部件和已有网格分页组件，再统一状态风格；保留路由与筛选业务语义，避免参数繁多的万能列表。

  完整路径：[E:/ppxb/jm-boom/web/src/routes/_app/explore/ranking.tsx:108](E:/ppxb/jm-boom/web/src/routes/_app/explore/ranking.tsx:108)；[E:/ppxb/jm-boom/web/src/routes/_app/explore/list.tsx:145](E:/ppxb/jm-boom/web/src/routes/_app/explore/list.tsx:145)；[E:/ppxb/jm-boom/web/src/routes/_app/explore/search.tsx:176](E:/ppxb/jm-boom/web/src/routes/_app/explore/search.tsx:176)；[E:/ppxb/jm-boom/web/src/features/downloads/page.tsx:80](E:/ppxb/jm-boom/web/src/features/downloads/page.tsx:80)；[E:/ppxb/jm-boom/web/src/components/empty-state.tsx:1](E:/ppxb/jm-boom/web/src/components/empty-state.tsx:1)。

## 原生 HTML 控件核查补充

以下清单用于说明原生控件的实际用途，不把所有原生 HTML 都计作缺陷。

| 位置 | 原生控件 | 审计判断 |
| --- | --- | --- |
| [E:/ppxb/jm-boom/web/src/features/reader/reader-settings-menu.tsx:199](E:/ppxb/jm-boom/web/src/features/reader/reader-settings-menu.tsx:199) | range input | 自绘浏览器皮肤与已有 Slider 重复，已列入 checklist。 |
| [E:/ppxb/jm-boom/web/src/features/reader/reader-settings-menu.tsx:228](E:/ppxb/jm-boom/web/src/features/reader/reader-settings-menu.tsx:228) | button | 方向选择位于 Menu 内，语义和键盘遍历问题已列入 checklist。 |
| [E:/ppxb/jm-boom/web/src/features/reader/reader-image.tsx:236](E:/ppxb/jm-boom/web/src/features/reader/reader-image.tsx:236) | button | 重试按钮独立写边框、字号与 hover，可随阅读器按钮规格整理复用 Button；原生 button 本身有效。 |
| [E:/ppxb/jm-boom/web/src/features/comic-detail/hero.tsx:179](E:/ppxb/jm-boom/web/src/features/comic-detail/hero.tsx:179) | button | 统计区的可点击评论块有专用布局和焦点样式；不应仅因原生标签要求替换。 |
| [E:/ppxb/jm-boom/web/src/features/reader/reader-hot-zones.tsx:17](E:/ppxb/jm-boom/web/src/features/reader/reader-hot-zones.tsx:17) | 两个 button | 透明翻页热区有独立用途，不应强套普通 Button 的皮肤。 |
| [E:/ppxb/jm-boom/web/src/components/ui/input.tsx:7](E:/ppxb/jm-boom/web/src/components/ui/input.tsx:7)、[E:/ppxb/jm-boom/web/src/components/ui/textarea.tsx:7](E:/ppxb/jm-boom/web/src/components/ui/textarea.tsx:7)、[E:/ppxb/jm-boom/web/src/components/ui/pagination.tsx:41](E:/ppxb/jm-boom/web/src/components/ui/pagination.tsx:41) | input / textarea / a | shadcn 基础组件底层使用原生元素是正常实现；分页问题在业务禁用行为。 |

业务页面没有发现额外自建的原生 select 或 textarea 表单控件。form、label、img、nav、section、div、span 等应按语义保留。已核实 scroll-fade-y、scroll-fade-x、scrollbar-none 能由当前样式依赖生成，不是无效 class。

## 已执行验证

第 14 项修复后重新执行了前端类型检查、lint、测试、构建、格式检查与 diff 检查。后端未改动，以下后端结果沿用第 4 项修复后的验证记录。

| 命令 | 结果 |
| --- | --- |
| web：bun x tsc --noEmit | 通过 |
| web：bun run lint | 通过 |
| web：bun run fmt:check | 未通过：其余 117 个现有 CRLF 源码文件仍被报告。本次修改和新增的 2 个每周推荐相关源码/测试文件单独运行 oxfmt 检查通过；未批量改写无关文件。 |
| web：bun run build | 通过 |
| web：bun test | 66 个测试通过，包含第 14 项新增的 8 个每周推荐页面挂载回归测试 |
| server：cargo test --locked | 78 个测试通过，含第 1 项新增的 7 个、第 2 项新增的 3 个、第 3 项新增的 3 个和第 4 项新增的 2 个回归测试 |
| server：cargo fmt --check | 通过 |
| server：cargo clippy --locked -- -D warnings | 通过 |
| 根目录：git diff --check | 通过 |

格式环境涉及 [E:/ppxb/jm-boom/web/.oxfmtrc.json:1](E:/ppxb/jm-boom/web/.oxfmtrc.json:1)；建议另行统一仓库换行约定，不为此次审计批量改写源码。

最小复现结果：末页禁用按钮处理仍将页码 3 改为 4；prefetchQuery 的 query 状态为 error 时外部 catch 未触发（第 12 项已改用 fetchQuery 修复）；反斜杠跳转路径通过现有检查并被 URL 解析为外站地址。复现未调用生产业务接口。

## 优化边界

继续保留现有下载 Manager/generation 与页面物化 single-flight，只有上文列出的明确缺陷需要局部修正。保留图片 64 MiB 限制、解码尺寸/像素/分配限制及原格式直通策略。API DTO/domain 映射不是本次要求删除的冗余。

没有规模或测量证据时，不将下载列表 clone、1 秒轮询或 Zustand 的独立 selector 认定为必须重写的性能问题，也不建议改为 SSE。字体构建输出包含 unicode-range 分片，不能把全部字体文件体积直接等同首屏下载量。

共 50 条审计记录，其中 13 条已修复、1 条待确认、36 条待处理；原生控件补充、验证记录和优化边界不计入条数。
