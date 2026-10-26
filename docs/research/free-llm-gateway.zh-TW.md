# 免費 LLM 轉接服務可行性研究與工程規劃

建立與查核日期：2026-10-26（Asia/Taipei）。狀態：初步研究；尚未執行供應商呼叫或瀏覽器橋接實測。

## 目標與初步判斷

評估是否能接受 REST 請求，將工作交給免費或低成本 LLM，再透過 REST 回傳結果；也納入串流、CLI、人工協作及瀏覽器橋接。重點是可用性、完整成本、限制與工程投入，包含使用者提出的「無頭橋接免費 Google Search AI Mode」。

**初步可行：官方免費 API 與本機模型可以作為第一階段；AI Mode 網頁橋接適合獨立驗證，尚不能列為穩定服務來源。** 免費額度不能推導成無限用量，也不能直接推導成可對外轉售或共享帳號。一般文字生成與即時搜尋問答須分別評分。

目前已確認的需求是：REST 接入、AI Mode 只需新建 session 或在原 session 續問並取得本次輸出，不抓取歷史；評估不登入的無頭操作；部署 VPS 為弱 CPU、1GB RAM。API 與遠端 browser worker 是資源取捨的候選，尚未選定實作方案。本機模型保留為其他硬體上的比較項，不列為這台 VPS 的優先部署方案。

本研究依同一標準比較供應商，不以 EDT 或其他公司的商業立場作為選型依據。下列來源支援的是公開產品規格；品質、可靠性與維護成本仍是待驗證的工程判斷。

## 評估邊界

先假設供個人或小型內部服務使用、文字輸入輸出、繁體中文為主要測試語言、付費自動備援關閉。對外公開服務、商業轉售、多使用者共享憑證及敏感資料處理須另行確認需求與供應商允許範圍。

「免費」分為四種：持續提供但有上限的官方免費層、會耗盡或到期的試用額度、消費者免費網頁介面，以及無逐次推論費的本機模型。不得把這四類合併成相同成本或服務承諾。

## 方案比較

### 圖片文字辨識最低需求

2026-10-26 使用者新增必要條件：至少能辨識圖片上有哪些文字。因此前述文字輸入輸出假設擴充為圖片加文字輸入、文字或 JSON 輸出；只支援文字的模型不能單獨滿足需求。第一階段以逐字轉錄為基準，保留原文與換行，不自動翻譯、摘要或補寫看不清的字；文字座標、表格版面還原與手寫辨識品質另行評估。

Groq 的 `qwen/qwen3.8-27b` 官方 vision 文件列出圖片輸入與 OCR 用途，免費限額表列 30 RPM、1,000 RPD、8,000 TPM、200,000 TPD。文件列每張圖計 2,048 input tokens，最多三張圖／請求。若每次一張圖、100 prompt tokens、500 output tokens，忽略額外推理及其他用量，理論上約 75 次／日；不能將 1,000 RPD 解讀為每日可辨識 1,000 張。此模型改列為 Groq OCR 原型候選，先前以 `gpt-oss-120b` 為例的文字 API 推薦不構成圖片能力的證據。[Vision 與 OCR](https://console.groq.com/docs/vision)、[免費限額](https://console.groq.com/docs/rate-limits)

Gemini 支援圖片理解，可作 OCR 對照；實作時選擇帳號當下可用且具免費圖片輸入額度的模型。Cloudflare 的 `llama-3.2-11b-vision-instruct` 可作備選，但先前 3B 純文字模型的免費次數估算不可套用到圖片模型。繁體中文、小字與密集文件的品質須使用相同圖片實測，尚無本專案的品質排名。[Gemini 圖片輸入](https://ai.google.dev/gemini-api/docs/image-understanding)、[Gemini 定價](https://ai.google.dev/gemini-api/docs/pricing)、[Cloudflare Vision](https://developers.cloudflare.com/workers-ai/models/llama-3.2-11b-vision-instruct/)

1GB VPS 優先走圖片上傳 → REST gateway → 雲端 vision API → 本次文字結果，不需本地載入視覺模型或 Chromium。請求新增 `task: ocr` 與圖片輸入；限制檔案大小、解碼像素數及併發，避免原圖、解碼圖及 base64 多份緩衝同時占用 RAM。必要縮圖須檢查小字是否仍可讀，不能只為省流量壓縮到不可辨識。使用模型的圖片用量估算，不能沿用純文字成本。

驗收增加有人工正解的繁中、英文、數字、螢幕截圖、拍照與模糊圖片；比較漏字、錯字、虛構文字、讀取順序、延遲與每張消耗。模型自報 confidence 不當成校準過的可信度；看不清的區域明確標示。免費帳號圖片配額、真實 OCR 品質及敏感圖片的資料政策仍待確認。

### 供應商概覽

| 方案 | 輸入與輸出 | 已查得費用或額度 | 主要限制 | 初步用途 |
| --- | --- | --- | --- | --- |
| Gemini API 免費層 | 官方 HTTP API，轉成統一 JSON | 部分模型輸入輸出免費；並非所有模型都可用 | RPM、TPM、RPD 隨模型與專案而異；實際值須讀帳號控制台。免費層資料使用政策需注意 | 一般生成候選 |
| Groq Free Plan | 官方 API | 公開表中 `openai/gpt-oss-20b` 為 30 RPM、1,000 RPD、8,000 TPM、200,000 TPD；帳號可能有例外 | 最先耗盡的限制生效；超額回 429；模型選擇會變動 | 第一個雲端 adapter 候選 |
| Cloudflare Workers AI | 官方 REST API | 每日 10,000 Neurons 免費；Paid 超額 US$0.011／1,000 Neurons | Neuron 不等於 token；部分模型需付費方式；Free 超額失敗，UTC 00:00 重設 | 有明確每日計量的候選 |
| OpenRouter 免費模型 | 統一 API | 有免費模型請求限額；本次擷取的額度數字不完整，暫不填舊數字 | 免費模型容量、供應商與資料政策各異；以帳號 `GET /api/v1/key` 與最新文件確認 | 次要備援與比較 |
| Ollama 本機模型 | 本機 HTTP API，可自行包 REST | 本機推論無供應商逐 token 費；硬體、電力及維運另計 | RAM／VRAM、模型授權、速度、中文品質；預設本機 API 不要求金鑰 | 可控性與資料留在本機的基準 |
| Gemini CLI 無頭模式 | 子程序輸入；JSON 或 JSONL 輸出；外包 REST | 是否免費與額度依登入方式和方案，須核對帳號 | 程序啟動、認證、版本、工具權限與配額；一個任務可能有多次模型呼叫 | 個人工具橋接候選 |
| Google Search AI Mode 瀏覽器橋接 | REST → browser worker → DOM 擷取 → JSON | 符合資格的免費網頁使用不代表免費 API 額度；主機與維護另計 | UI、登入、驗證、地區、完成判定、引用擷取及服務規則；無可據以承諾的吞吐量 | 獨立實驗，不列第一階段可靠備援 |
| Gemini API 加 Google Search grounding | 官方 API，答案與引用 | 搜尋另有模型／方案相關額度與收費 | 不等同 AI Mode 網頁產品；必須計入模型及搜尋費，保留必要引用／展示資訊 | 搜尋問答的官方對照組 |

規格來源：[Gemini 定價](https://ai.google.dev/gemini-api/docs/pricing)、[Gemini 限流](https://ai.google.dev/gemini-api/docs/rate-limits)、[Groq 限流](https://console.groq.com/docs/rate-limits)、[Cloudflare 定價](https://developers.cloudflare.com/workers-ai/platform/pricing/)、[OpenRouter 限額](https://openrouter.ai/docs/api-reference/limits)、[Ollama API](https://docs.ollama.com/api/introduction)、[CLI 無頭模式](https://geminicli.com/docs/cli/headless/)、[CLI 配額](https://geminicli.com/docs/resources/quota-and-pricing/)、[AI Mode 說明](https://support.google.com/websearch/answer/16011537)、[Search grounding](https://ai.google.dev/gemini-api/docs/google-search)。

Gemini 定價頁將免費層標示為可能用於改善產品的資料處理方式，實際適用情形需連同[附加條款](https://ai.google.dev/gemini-api/terms)核對；初期樣本使用公開或合成資料。其他供應商的保留、訓練用途、地區與再提供服務條件也須逐一核對，不能因提供 API 就推定所有用途獲准。

## Google Search AI Mode 橋接專項

### 要驗證的架構

`REST 請求 → 持久佇列 → 專用瀏覽器工作程序 → AI Mode → 答案與引用擷取 → 結果儲存 → REST 查詢`

官方說明描述的是搜尋互動體驗，部分問題可能只得到網頁連結，不能把「有頁面內容」直接判定為成功生成答案。本次查到的文件未確認有公開、免費且等同 AI Mode 網頁功能的官方 API；這是目前的證據範圍，不是斷言不存在任何 API。

### 三種橋接方式

| 方式 | 工程特性 | 待驗證與限制 |
| --- | --- | --- |
| Headless browser | 可在背景部署；直接讀畫面可見內容與引用 | 無頭環境是否可用、登入持續性、動態 DOM、回答完成訊號與記憶體用量 |
| 有視窗的專用 browser worker | 較容易由人處理登入或查看失敗畫面 | 需桌面環境；人工介入後仍要恢復工作與防止重複送出 |
| 人工完成搜尋後回填結果 | 可先驗證結果格式、引用與工作流程 | 不屬於全自動；人力成本與等待時間必須列入 |

無頭化只改變瀏覽器執行方式，不會消除使用限制。Google 通用條款涉及禁止繞過保護措施及違反機器可讀指示的自動存取；測試前需核對適用地區版本、服務附加規則與目標頁面指示，不能僅憑通用條款判定整項工程合法或違規。[Google 條款](https://policies.google.com/terms)

實驗優先驗證未登入、低併發的匿名 browser context；登入不是預設必要條件。遇強制登入、CAPTCHA 或禁止存取時標記 `needs_attention` 並停止自動重試，不自動切換使用者帳號，也不將代理輪替、多帳號擴額或驗證繞過納入設計。Cookie 與 profile 不進 Git。公開服務或轉售是否可行另作決策。

成功條件須包含：取得本次問題的完整答案、保留對應引用 URL、無前一筆會話污染、能識別只回搜尋連結／拒答／逾時，以及程序重啟後能辨識未完成工作。回覆穩定一小段時間只能作啟發式訊號，不能單獨代表回答已完成。

### 未登入與無頭模式的可行性

不登入和無頭執行是兩個獨立條件。Google 的登入說明將 AI Mode 歷史紀錄及影像生成等功能列為登入後的優勢，不能據此認定基本問答必須登入；該文件也沒有承諾未登入的無頭自動化可用。以目前證據只能列為待驗證方案，不能宣稱已可穩定運作。[Google 登入說明](https://support.google.com/websearch/answer/16985229)、[AI Mode 使用說明](https://support.google.com/websearch/answer/16011537)

匿名續問仍需保留該 browser context 的 Cookie、儲存及頁面執行狀態。不登入不等於每次請求清除狀態，也不代表不使用 Cookie。續問使用原 context 與原頁面；新 session 使用新的隔離 context，並從 AI Mode 新對話入口開始。context 隔離的是瀏覽器狀態，不代表取得新配額或保證 Google 內部的識別方式。

最小驗證順序：在實際部署地區與網路上，以未登入的無頭瀏覽器開啟 AI Mode，送出第一個問題、取得回答，再在原頁面連續追問兩次；結束後建立新 context，確認可開始新對話。檢查是否強制登入、是否提供追問欄、能否擷取當次完整輸出及是否出現驗證。匿名有視窗操作只作診斷對照，不能取代無頭測試通過。VPS 的 OS、CPU 架構、實際可用記憶體及來源網路仍待確認。

### Session 請求與生命週期

2026-10-26 需求確認：只提供兩種情境：在同一個 session 輸入並取得本次輸出，或開啟新 session。不抓取、匯出、摘要、雜湊或重播前面的歷史內容；既有對話上下文由 AI Mode 自行管理。此處 session 指 AI Mode 對話，不是登入 Cookie。

| 操作 | 請求草案 | 行為與回傳 |
| --- | --- | --- |
| 開啟新 session | `POST /v1/llm/sessions`，可帶第一個 `input` | 透過實際介面的新對話／新搜尋入口建立對話，綁定專用頁面並回傳本服務的 `session_id`；有輸入時只擷取該次輸出 |
| 同一個 session 續問 | `POST /v1/llm/sessions/{session_id}/messages`，帶 `input` | 在綁定頁面的 follow-up 輸入區送出，只等待並擷取本次新增的答案與引用，回傳原 `session_id` 與本次結果 |

`session_id` 是本服務的識別碼，對應存活的 browser worker、頁面 handle 與必要的對話定位資訊，不冒稱 Google 官方 session ID。建立新 session 不會覆寫舊 session 的綁定。同一個 session 的請求依序執行，避免兩次輸入及輸出交錯。

worker 維持頁面在該對話；透過頁面生命週期、導航事件及實際可取得的對話定位資訊偵測離開或失效，不讀取舊問答作核對。送出前設定本次輸出觀測起點，只監看新答案的容器與完成狀態；具體 DOM 定位與完成訊號須由 PoC 驗證，不能以整頁文字作結果。

第一版範圍為存活 worker 內的 session。頁面遺失、意外導頁、登入失效或 browser 重啟後無法確認綁定時，明確回報 `session_lost`，由呼叫端決定是否開新 session，不默默替換。送出後斷線且無法判定本次結果時回報 `unknown`，不自動重送。跨重啟恢復與歷史紀錄功能不在本次需求內。

PoC 驗收聚焦：新建 A 後連續兩次續問均留在 A、回傳內容只含本次答案、同 session 請求依序執行，以及失效時不自動改開新對話。1GB 配置先釋放 A 再建立 B，舊 ID 必須失效；多 session 相互隔離僅在日後提高容量時驗證。不得要求抓取舊內容作為驗收前置條件。無頭流程尚待實測，以上為設計契約。

### 官方搜尋對照組

Gemini Search grounding 會執行搜尋並提供來源，但不能假設其答案、排名、會話能力與 AI Mode 相同。官方文件目前列 Gemini 3 及更新模型的搜尋計價為每月共用 5,000 次免費搜尋請求，之後 US$14／1,000 次；單次 prompt 可能觸發多次搜尋。這是搜尋工具計價，**不是整次 LLM 呼叫免費**，且需確認所選模型及方案適用性。[搜尋計價說明](https://ai.google.dev/gemini-api/docs/google-search)

## 成本模型與容量

### 1GB RAM VPS 的部署限制

2026-10-26 使用者確認：VPS CPU 較弱、RAM 只有 1GB。最省 VPS 資源的主方案是 REST gateway 呼叫官方 API；若必須取得 AI Mode 網頁結果，可將 browser worker 放在已有且可運作的另一台電腦，VPS 僅處理工作與結果。遠端 worker 建議主動向 VPS 領取工作與提交結果，使用驗證與 TLS，不公開瀏覽器除錯介面。另一台電腦的電力、在線時間及維護仍是成本，不能視為免費常駐運算。

若限定所有元件都在這台 VPS，AI Mode 採單一 Chromium instance、單一匿名 context、單一頁面、併發 1 的探索配置。共用 browser instance 可避免每筆工作重開瀏覽器；context 可隔離 Cookie 與儲存狀態，但不代表頁面沒有 RAM 成本。[Playwright contexts](https://playwright.dev/docs/browser-contexts)

此低記憶體配置最多保留一個活動 session。續問維持原頁面；新建第二個 session 時若舊 session 仍在使用，先回報容量不足，由呼叫端明確釋放舊 session 後再建立，不能默默取代舊綁定。閒置期限初始可設 5 分鐘（可調整的測試值），只在沒有執行中工作時關閉 context；回傳 `expires_at`，到期後舊 ID 回報 `session_lost`。全部 session 關閉後才可關閉 browser 釋放記憶體；每次問完就關閉會破壞續問需求。

為維持只有兩種業務操作，新建請求可另帶明確的 `replace_session_id`，表示結束該舊 session 後開新 session；這是請求草案，尚未實作。舊 session 正在執行或已有排隊續問時先回報忙碌，不中斷或搬移工作。未指定替換且容量已滿時回報 `session_capacity_exceeded`。不需另建歷史查詢功能。

資源措施：不安裝桌面／VNC；使用 library worker，不啟動測試 runner；平常不錄影、截圖或追蹤；事件式等待取代密集整頁輪詢；佇列限制長度與輸入大小；二進位在別處編譯後部署。先建立正常載入基準，再嘗試阻擋圖片、影音及字型並比較成功率，保留 JavaScript、fetch/XHR 與必要樣式。不要假定 headless shell 或特定 Chromium flags 必定降低記憶體且相容，也不以關閉 sandbox 節省資源。

1GB 能否容納實際 AI Mode 頁面尚無量測結果。測試須觀察整個 worker 程序樹／cgroup 的峰值記憶體、系統 MemAvailable、swap 活動、CPU 與延遲，而非只看 Rust 或 Node 主程序。可暫以保留至少 200MiB 系統可用記憶體作初步觀察門檻（不是保證）；若反覆接近 OOM、持續換頁或逾時，改用遠端 browser worker 或 API。swap 只能作突發緩衝，不能當穩定容量；弱 CPU 上也不預設壓縮記憶體一定划算。

月總成本 = 模型 API ＋搜尋工具＋主機／電力＋儲存／網路＋維護工時×時薪＋人工處理工時×時薪＋硬體月攤提。

以下為比較用假設，不是實測或供應商報價：每次 2,000 input tokens、500 output tokens，每月 30 天；對話歷史、重試及推理 token 會增加用量。

| 流量情境 | 每月請求 | 輸入／輸出量 | 判讀 |
| --- | --- | --- | --- |
| 20 次／日 | 600 | 120 萬／30 萬 tokens | 免費 API 原型合理，但仍受尖峰限流影響 |
| 100 次／日 | 3,000 | 600 萬／150 萬 tokens | 需同時檢查每日 token 與 request 限額 |
| 1,000 次／日 | 30,000 | 6,000 萬／1,500 萬 tokens | 不應預設免費層可以支撐 |

Groq 上列模型若以每次總量 2,500 tokens 粗估，200,000 TPD 只約支撐 80 次／日，會早於 1,000 RPD 耗盡；實際計數、輸出預留與推理消耗以帳號及回應為準。這說明僅比較 RPD 容易高估容量。

Cloudflare 以 `llama-3.2-3b-instruct` 公開換算率 4,625 input／30,475 output Neurons 每百萬 tokens 計算，同一假設每次約 24.49 Neurons，每日約 408 次；這是忽略其他用量的理論上限，不是品質、吞吐或服務保證。[模型換算表](https://developers.cloudflare.com/workers-ai/platform/pricing/)

本機電費示例：假設平均新增功耗 100 W、每日 8 小時、電價 NT$4／kWh，約 NT$96／月；電價僅為計算假設，不是現行台電費率。還需硬體折舊與待機消耗。瀏覽器橋接若每月維護 2 小時、時薪假設 NT$800，維護就需 NT$1,600，可能高於低量付費 API。

付費對照統一按 `月輸入 tokens / 1e6 × 輸入單價 + 月輸出 tokens / 1e6 × 輸出單價 + 搜尋費` 計算。搜尋若月用 6,000 次且符合上述 5,000 免費額度，搜尋部分為 US$14；模型費另加。初版不把任何付費方案設為自動 fallback。

## REST 與其他輸出機制草案

以下為待實作設計，並非目前 Courier Hub 已提供的端點。

| 機制 | 設計 | 用途 |
| --- | --- | --- |
| 同步 REST | `POST /v1/llm/responses`，完成回 200 JSON | 短且可預期的 API 推論 |
| 非同步 REST | `POST /v1/llm/jobs` 回 202 與工作 ID；`GET /v1/llm/jobs/{id}` 取結果 | 預設方案，適合限流等待與 browser worker |
| SSE | 後續提供工作事件與文字串流 | 改善等待體驗；不要冒充可恢復的完整結果 |
| Webhook | 工作完成後簽章通知，另行設計重試與目的地限制 | 客戶端不方便 polling 時再加入 |
| CLI JSON／JSONL | 子程序 stdin／參數輸入，stdout 解析 | 接入官方 CLI；避免拼接 shell 命令 |

上述為全體方案的候選傳輸方式；AI Mode 第一版只暴露前述「新建 session／續問」業務操作，長時間執行可回 202 與 `job_id`，再透過工作結果端點取得本次輸出。`session_id` 指對話，`job_id` 指單次執行，不能混用。工作結果的短期保存不代表從 Google 擷取舊對話歷史；不提供歷史回填或完整會話匯出。SSE、Webhook 不是目前第一版必要項目。

請求至少包含 `task`（`generate` 或 `search_answer`）、`input`、`provider_policy`、`max_output_tokens`、`deadline_seconds`；以 `Idempotency-Key` 防重複入列。第一版使用服務端允許清單選 provider 與 model，不讓外部輸入指定任意上游 URL。

結果至少包含 `id`、`status`、實際 `provider`／`model`、`output_text`、`citations`、`usage`、`cost`、`error` 與時間戳。無法取得的 tokens／費用填 `null`，不可填 0 冒充免費。引用需保留來源 URL；不同搜尋供應商必要的展示資料另外保留。

狀態建議為 `queued → running → succeeded / failed / needs_attention / unknown`，另支援期限到期。上游逾時但可能已執行時標為 `unknown`；重試可能再次扣額，不能承諾 exactly once。429 依 `Retry-After` 與 deadline 排程，耗盡每日額度時等待或使用事先允許的備援。

統一介面不代表功能完全相容：adapter 需宣告搜尋、串流、結構化輸出及上下文能力；不得把需要即時來源的工作默默降級為離線生成。輸出 JSON 要做 schema 驗證，不能只靠 prompt 要求。

## 與 Courier Hub 的關係

現有專案已有 Axum、Tokio、SQLite 工作佇列及 API 驗證模式，可作為設計參考。現有 `DeliveryTransport::deliver` 的輸入是 Email，現有工作查詢也不回傳 email 內容；不宜直接把 LLM 回覆塞入郵件工作。

第一階段建議建立獨立 LLM provider 介面、DTO、結果儲存與 worker；是否共用 crate 或拆成另一個服務，在 PoC 後決定。LLM 結果讀取須做擁有者授權、保留期限與刪除；一般日誌不記完整 prompt、Cookie 或金鑰。本次只新增研究紀錄，尚未修改正式 API 或資料庫。

## 分階段驗證與決策門檻

以下工時為熟悉 Rust／HTTP 開發者的初估，不含帳號申請等待、GPU 採購及正式產品化。

| 階段 | 產出 | 初估 | 完成門檻 |
| --- | --- | --- | --- |
| P0 需求與額度確認 | 日用量、尖峰、VPS OS／架構及可用 RAM、資料類型、免費定義；需帳號來源再查配額 | 0.5–1 人日 | 所選來源具可用存取方式，成本與資料政策可接受 |
| P1 API 最小原型 | 一個官方免費 API adapter、REST job 與結果；本機資源允許時加 Ollama | 1–3 人日 | 端到端可回覆；限流、deadline、重啟與重複提交可辨識 |
| P2 CLI 與 AI Mode 對照 | 優先測匿名無頭新建／續問與 1GB 峰值資源；有視窗及 CLI 作其他候選對照 | 2–5 人日探索上限 | 兩種操作可用、只取本次結果、可分類失敗且不依賴繞過保護措施 |
| P3 選型 | 品質、成功率、P50/P95 延遲、人工介入與每次成功成本報表 | 1–2 人日 | 確認主方案、備援、不可支援項目與月預算 |

測試集先用 30 題固定公開／合成樣本：10 題繁中摘要改寫、10 題 JSON 擷取、10 題需要最新資料且可核對來源的問答。所有方案使用相同題目；不支援搜尋的方案在搜尋項標為不適用。配額內分日重跑，記錄模型版本、時間、延遲、usage、429、空答案、截斷、引用完整度及人工處理分鐘數。

原型門檻暫訂：支援項目有效完成率至少 95%、JSON schema 通過率至少 95%、搜尋來源逐筆可追溯；目標延遲待需求確認。30 題僅可初篩，不能證明 95% 生產 SLA。進入長期服務前再做至少七天低量運行觀察，檢查 quota reset、匿名狀態失效、記憶體成長及頁面變動；AI Mode 重啟後驗證舊 session 明確失效與未完成工作可辨識，不要求恢復原對話。登入過期僅適用於另行選用的登入方案。

若 AI Mode 在探索上限內仍無法可靠判定完成、需要頻繁人工介入，或使用方式與適用規則不相容，就停止其自動化分支；保留人工研究用途及官方 grounding 對照。採用與否依成功成本和需求決定，不因表面 API 費為零而優先。

## 待辦與研究日誌

- [x] 建立持續紀錄位置與首版比較。
- [x] 查核主要官方 API、CLI、本機 API 與 AI Mode 文件。
- [x] 確認 session 只需新建與續問，且不抓取前面歷史。
- [x] 記錄不登入與無頭模式的證據範圍及驗證流程。
- [x] 記錄 VPS 為弱 CPU、1GB RAM，以及單 session 省資源配置。
- [ ] 確認個人／內部／公開服務、日流量、最大等待時間及每月預算。
- [ ] 確認是否必須取得「AI Mode 本身的結果」，或只需可附來源的搜尋問答。
- [ ] 確認 VPS OS／CPU 架構、實際可用 RAM、部署地區與既有服務負載；確認有無可用遠端 worker 主機。
- [ ] 確認需帳號供應商的實際額度及地區可用性。
- [ ] 實測匿名無頭首次問答與原 session 連續追問，再測新 context 開新 session。
- [ ] 量測整個 browser worker 的峰值 RAM、CPU、swap 與延遲，判定是否適合留在 1GB VPS。
- [ ] 確認閒置期限、替換舊 session 的請求契約與忙碌／失效錯誤回應。
- [ ] 逐一核對商用、再提供服務、資料保留與模型授權條件。
- [ ] 執行 P1；保存不含秘密的測試摘要。
- [ ] 執行 P2，記錄是否登入、browser／CLI 版本與人工介入頻率。
- [ ] 比較總成本後決定是否新增正式 LLM 模組。

| 日期 | 新證據或決策 | 下一步 |
| --- | --- | --- |
| 2026-10-26 | 初步確認官方免費 API、本機 HTTP 與 CLI JSON 均有接入路徑；AI Mode 無頭橋接尚未驗證 | 先確認需求與帳號配額，再執行小型 PoC |
| 2026-10-26 | 使用者將 AI Mode 範圍收斂為新建與同 session 續問，只取本次輸出；不抓歷史 | 移除歷史比對、雜湊、重播及跨重啟恢復要求 |
| 2026-10-26 | 納入未登入無頭方案；修正先前把登入帳號當必要條件的假設 | 在實際部署網路驗證匿名進入、續問及驗證阻礙 |
| 2026-10-26 | VPS 確認為弱 CPU、1GB RAM；記錄 API、遠端 worker、單 browser／單 session 三種部署取捨 | 實測峰值資源再選型；5 分鐘 TTL 等數值仍為提案 |

後續每次更新記錄查核日期、來源連結、帳號適用範圍及結果；舊配額和估算應明確標示已被取代。所有端點、工時與驗收門檻在正式決策前均屬規劃。
