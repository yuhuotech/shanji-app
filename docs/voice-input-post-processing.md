# 语音输入后处理方案

## 1. 背景与目标

Shanji 当前做的不是“逐字稿系统”，而是“语音输入系统”。

这两者的最终目标不同：

- 逐字稿：尽量忠实保留原话，包括口头禅、停顿、重复和不完整表达
- 语音输入：最终输出应接近用户原本想输入到文本框里的文字

因此，单纯把 ASR 识别结果直接展示或粘贴出来是不够的。高质量语音输入需要把识别结果继续经过多层处理，分别解决：

- 低延迟实时反馈
- 错字、漏字、重复字
- 口语化、语气词、卡顿
- 断句和标点
- 最终文本可读性和可输入性

本方案的目标是建立一条完整且可扩展的本地语音输入处理链路：

1. 实时显示由流式模型承担
2. 已稳定语音段由整模型做纠错
3. 文本规范化和标点恢复独立成层
4. LLM 润色作为可选增强，不强制依赖
5. 全过程可配置、可追溯、可回放

## 2. 产品设计

### 2.1 用户可感知的工作模式

系统分为三层输出：

- 实时文本
  - 目标是低延迟反馈
  - 允许存在少量错字和不稳定内容
- 纠正文本
  - 在用户说话停顿后，对稳定 segment 进行整模型纠错
  - 这是默认最终文本的主要来源
- LLM 润色文本
  - 仅在用户开启并配置可用 LLM Provider 后启用
  - 用于进一步去口语、去重复、补自然表达和整理全文

### 2.2 当前产品策略

- 实时 ASR 默认使用 `paraformer-zh-streaming`
- 整体纠正默认关闭，用户可单独开启
- LLM 润色默认关闭，用户配置完成后可开启
- 标点恢复当前为独立的轻量规则层，不依赖单独 `punc` 模型

### 2.3 为什么不直接把所有问题都交给 ASR

ASR 模型主要解决“音频到文字”的问题，不擅长稳定处理：

- 语气词清理
- 去重
- 口吃和卡顿合并
- 数字、英文、品牌名规范化
- 自然断句和输入式书面化

因此最终语音输入效果必须由“识别 + 文本后处理”共同完成。

## 3. 总体处理链路

当前链路如下：

1. 音频采集
2. 降噪 / VAD
3. 流式 ASR 实时识别
4. segment 切分
5. 整模型 segment 纠错
6. 文本规范化
7. 标点恢复
8. 可选 LLM 润色
9. 剪贴板输出 / 自动粘贴
10. 历史记录落库与录音保存

### 3.1 分层职责

#### 层 1：实时流式识别

职责：

- 尽快给浮窗出字
- 维持低延迟体验
- 提供 segment 的初始文本

实现：

- 模型：`paraformer-zh-streaming`
- 代码入口：
  - [audio_transcriber.rs](/Users/hmw/data/app/shanji-rust/crates/shanji-slint/src/audio_transcriber.rs)
  - [asr.rs](/Users/hmw/data/app/shanji-rust/crates/shanji-core/src/asr.rs)
  - [paraformer.rs](/Users/hmw/data/app/shanji-rust/crates/shanji-core/src/asr/paraformer.rs)

#### 层 2：整模型纠错

职责：

- 对已经稳定的 segment 重新识别
- 纠正流式阶段的错字、漏字、尾字不稳、同音词问题

实现：

- 模型：`paraformer-zh`
- 运行方式：
  - 说话停顿后提交 segment
  - 后台 worker 异步做整模型重识别
  - 回写到对应 segment，不覆盖用户正在说的话

#### 层 3：文本规范化

职责：

- 去除语气词
- 清理重复字和重复短语
- 基础技术词与品牌名规范化
- 中英文和空白符清理
- 常见识别错误纠正

实现：

- 独立模块：[text_processing.rs](/Users/hmw/data/app/shanji-rust/crates/shanji-core/src/text_processing.rs)

#### 层 4：标点恢复

职责：

- 把规范化后的文本整理成可读的句子
- 当前只做轻量断句和句末补全

实现：

- 与文本规范化同模块实现
- 当前不接 `punc` 模型
- 标点恢复是独立层，但当前是规则版

#### 层 5：可选 LLM 润色

职责：

- 修正明显识别错误
- 去口语、去赘余、去卡顿
- 补更自然的书面表达
- 让最终内容更适合直接输入或粘贴

实现：

- 使用已有 LLM Provider 体系
- 仅在用户启用并配置 Provider 时运行
- 当前文案与默认 prompt 已切换为“语音输入润色”

## 4. 当前实现细节

### 4.1 Segment 模型

运行时不再把整段录音当成一个黑盒字符串，而是拆成一系列 `segment`：

- `live_text`：流式模型实时识别结果
- `corrected_text`：整模型纠正结果

这样带来几个好处：

- 实时显示不会因为整体纠错而大幅跳动
- 已完成语句可逐段变好
- 停止录音后的等待时间更短
- 历史记录可以区分原始实时文本与纠正文本

### 4.2 Pending Segment 与短段合并

为了避免过短停顿导致切段过碎，当前实现加入了：

- `PendingSegment`
- 最短纠正段阈值 `MIN_REFINE_SEGMENT_SAMPLES`
- segment overlap `SEGMENT_OVERLAP_SAMPLES`

逻辑：

- 太短的语音段先不提交，保留为 pending
- 后续音频与 pending 合并后再统一提交
- segment 结束时保留一小段 overlap，减轻边界漏字

### 4.3 双阈值端点检测

当前 VAD 已从单阈值方案升级为双阈值端点检测：

- `vad_threshold`
  - 语音启动阈值
- `vad_end_threshold`
  - 语音结束阈值
- `min_speech_frames`
  - 最少连续起说帧数
- `silence_timeout_ms`
  - 尾部拖尾时长

这样做的目的不是单纯“更严格”，而是降低这几类问题：

- 噪声误触发
- 说话刚开始就启动过早
- 句中轻微停顿导致过碎切段
- 尾字被截断

当前 VAD 逻辑还保留了 pre-buffer 和 tail-buffer，因此在提高稳定性的同时不会明显增加字头字尾丢失。

### 4.4 文本规范化规则

当前已实现的规则包括：

- 去除常见语气词
  - 如 `嗯`、`啊`、`呃`、`哦`
- 清理重复短语
- 清理重复字符
- 移除 BPE 残留标记 `@@`
- 基础技术词规范化
  - 如 `xcode -> Xcode`
  - `ios -> iOS`
  - `github -> GitHub`
- 中英文混合空格规范化
- 常见误写修正

### 4.5 热词 / 词典层

当前已把仓库里原有的热词库基础设施正式接入语音输入主链路。

热词库现在承担两类作用：

1. 作为规范化词典
   - 参与技术词、品牌词、英文大小写的标准化
2. 作为后续识别增强的语义词表
   - 当前主要用于后处理
   - 后续可继续扩展到解码偏置和热词加权

系统启动时会自动确保存在一份内置技术词库，例如：

- `Apple`
- `Intel`
- `Mac`
- `MacBook`
- `Xcode`
- `iOS`
- `GitHub`
- `ModelScope`
- `Hugging Face`
- `ONNX`
- `Paraformer`
- `FunASR`

这样即使用户还没有手动导入词库，技术类语音输入也能立即得到基础词典支持。

### 4.6 ITN 第一版

当前已加入第一版逆文本规范化（ITN）能力，重点覆盖语音输入里最常见的高价值表达：

- 年份
  - `二零二五年` -> `2025年`
- 百分比
  - `百分之九十五` -> `95%`
- 金额 / 数量后缀
  - `一千八百元` -> `1800元`
  - `八百 MB` 类表达会优先整理成数字加单位

这还不是完整 ITN，但已经足以覆盖大量日常输入场景。

### 4.7 标点恢复规则

当前标点恢复是独立层，但属于轻量版本，主要策略是：

- committed segment 之间优先使用逗号连接
- segment 间长停顿可升级为句号
- 对常见连接词前增加轻量停顿
- 仅在整次会话 final 阶段补全句末句号或问号

当前标点恢复已经开始利用端点层提供的停顿信息，而不再是纯文本启发式。

该设计仍然刻意保持保守，避免实时阶段标点频繁抖动。

### 4.8 LLM 润色触发点

LLM 润色不进入实时主链路，而是在最终文本生成后触发。

原因：

- 避免增加实时延迟
- 避免频繁网络请求
- 避免实时文本被外部模型反复重写
- 让 LLM 只处理“已经稳定的整段文本”

当前默认 prompt 目标为：

- 适合直接输入或粘贴
- 修正明显识别错误
- 去除语气词、重复和口吃
- 补自然标点
- 保留原意，不无端扩写

## 5. 配置设计

### 5.1 ASR 配置

当前 ASR 配置位于 [config.rs](/Users/hmw/data/app/shanji-rust/crates/shanji-core/src/config.rs)：

- `liveModelId`
  - 默认 `paraformer-zh-streaming`
- `refineEnabled`
  - 是否启用整模型纠正
- `refineModelId`
  - 默认 `paraformer-zh`
- `insertPunct`
  - 是否启用标点恢复层
- `punctStyle`
  - 当前支持 `zh` / `en`

### 5.2 Rewrite / LLM 配置

当前 LLM 润色沿用原有 Rewrite 配置：

- `enabled`
- `activeProviderId`
- `providers`
- `activePromptId`
- `prompts`

默认 prompt 已从“智能改写”收敛为“语音输入润色”。

## 6. 状态与输出

### 6.1 状态机

当前主状态包括：

- `Idle`
- `Recording`
- `Transcribing`
- `Rewriting`

### 6.2 最终输出优先级

当前最终文本输出优先级为：

1. `LLM 润色文本`，如果开启且成功
2. `整体纠正文本`，如果存在
3. `实时文本`

### 6.3 剪贴板与自动粘贴

最终文本会经过 `output` 模块格式化后：

- 写入剪贴板
- 在需要时模拟粘贴
- 可选恢复原剪贴板内容

## 7. 历史记录与可追溯性

当前历史记录会保留：

- `transcribed`
- `live_transcribed`
- `corrected_transcribed`
- `rewritten`
- `live_model_id`
- `refine_model_id`
- `refine_enabled`
- `provider_id`
- `audio_path`
- `duration_ms`

这意味着每一次语音输入都可以追溯：

- 实时识别出来了什么
- 整模型纠正成了什么
- LLM 最后又改成了什么
- 使用的是哪个模型与哪个 Provider
- 原始录音文件在哪里

这是后续定位效果问题和回归问题的重要基础。

## 8. 为什么当前先不接 punc 模型

当前明确不接 `punc` 模型，原因是：

- 先把“流式识别 + 整模型纠正 + 文本规范化 + LLM 润色”的主链路做稳定
- 减少运行时模型数量与下载成本
- 避免一次引入过多新变量，增加定位难度

但架构上已经为 `punc` 模型保留了位置：

- 它将来应处于“文本规范化之后、LLM 润色之前”
- 最合适的触发点是：
  - 已提交 segment 的纠正后文本
  - 或整次会话 final 文本

## 9. 当前局限

当前版本仍有这些已知局限：

- 标点恢复仍是规则版，不是学习型模型
- 文本规范化规则仍偏保守，尚未覆盖更强的 ITN 能力
- 领域词、专有名词、联系人名等还没有热词纠错层
- LLM 润色仍属于“最终增强”，不参与 segment 级回写
- 短段合并和 overlap 阈值仍需要根据真实使用继续调参

## 10. 后续演进建议

推荐按如下顺序继续演进：

1. 完善文本规范化规则
   - 数字、日期、金额、单位
   - 中英文混排
   - 常见品牌与技术词

2. 引入热词 / 领域词典
   - 联系人名
   - 地名
   - 公司名
   - 产品名

3. 引入可选 `punc` 模型
   - 用于替代当前规则版标点恢复

4. 引入更强的 final polish 模式
   - LLM 仅在用户开启时参与
   - 可区分“轻度润色”和“正式写作”

5. 按场景提供输出模式
   - 原话模式
   - 输入模式
   - 正式写作模式

## 11. 对应代码位置

- 实时转写主链路
  - [audio_transcriber.rs](/Users/hmw/data/app/shanji-rust/crates/shanji-slint/src/audio_transcriber.rs)
- ASR 引擎与模型加载
  - [asr.rs](/Users/hmw/data/app/shanji-rust/crates/shanji-core/src/asr.rs)
- Online Paraformer 流式实现
  - [paraformer.rs](/Users/hmw/data/app/shanji-rust/crates/shanji-core/src/asr/paraformer.rs)
- 文本后处理与标点恢复
  - [text_processing.rs](/Users/hmw/data/app/shanji-rust/crates/shanji-core/src/text_processing.rs)
- 应用配置
  - [config.rs](/Users/hmw/data/app/shanji-rust/crates/shanji-core/src/config.rs)
- LLM Provider 与默认 prompt
  - [llm.rs](/Users/hmw/data/app/shanji-rust/crates/shanji-core/src/llm.rs)
- 历史记录
  - [history.rs](/Users/hmw/data/app/shanji-rust/crates/shanji-core/src/history.rs)

## 12. 本轮落地结论

本轮完成的不是单点修补，而是把 Shanji 的语音输入主链路正式收敛为：

- 流式实时识别
- 整模型分段纠错
- 独立文本规范化
- 独立标点恢复层
- 可选 LLM 润色
- 全链路历史追踪

这为后续继续引入 `punc` 模型、热词系统、领域词典和更强的最终文本整理打下了稳定结构基础。
