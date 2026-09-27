# 实现说明：nested 集合索引在 schema 错误中丢失

## 问题与复现

注入场景：`Vec` 中第二个对象的字段验证与 schema 级错误同时发生，合并后
schema 错误覆盖或脱离了元素索引。修复前在库级别可稳定复现三类缺陷：

1. `ValidationErrors::merge(Ok(()), "children", vec.validate())` 把集合的
   `List` 包进 `Struct`，索引被埋进内部载体键 `_tmp_validator` 之下，
   输出形如 `{"children": {"_tmp_validator": {"1": {...}}}}`——
   list 索引脱离了字段路径（`merge_self` 会剥离载体键，`merge` 不会，两者不一致）。
2. `ValidationErrors::merge_all(Ok(()), "children", children.iter().map(|c| c.validate()).collect())`
   直接返回 `Ok(())`：未被 `merge(.., field, ..)` 预包装的元素结果被静默丢弃，
   第二个元素的字段错误与 `__all__` schema 错误连同索引一起丢失。
3. 序列化往返（`to_string` → `from_str`）后 `List` 退化为 `Struct`：
   untagged 反序列化优先匹配 `Struct`，`{"1": {...}}` 的索引类型丢失。

## 可选方案与最终取舍

### 方案一：schema 错误挂在容器还是元素

- **A. 提升到容器**：把元素的 `__all__` 上提到父级 `__all__`。
  否决——元素索引随之丢失，正是本 bug 的表现；且会把 schema 错误伪装成
  容器的字段级错误（任务明确禁止）。
- **B. 留在产生它的层级（采用）**：schema 错误挂在执行该 schema 校验的对象
  自身的 `__all__` 键下；合并函数只做路径附着，不在容器与元素之间搬运
  `__all__`。元素级结构为 `children: List({1: {name: Field, __all__: Field}})`，
  字段、list 索引、struct 级错误在同一棵树内共存，且可从现有接口
  （`ValidationErrorsKind` 匹配、`field_errors()`、`errors()`）逐一区分，
  不依赖任何隐藏全局状态，默认配置语义不变。

### 方案二：重复 code 的保留结构

- **A. 按 code 去重/覆盖**：否决——相同 code 的多次出现（多个 schema、
  length+custom 同 code）会被合并，错误数量与顺序失真。
- **B. 保留有序重复项（采用）**：`Field(Vec<ValidationError>)` 天然按产生
  顺序保留重复 code；`add` 追加而非覆盖。稳定结果：同一字段/同一 `__all__`
  下两个相同 code 的错误保持 `[第一个, 第二个]` 的顺序与各自 params，
  错误位置（字段键或 `__all__`）不变。回归测试：
  `validator/tests/merge.rs::duplicate_codes_keep_separate_entries`、
  `validator_derive_tests/tests/nested_schema.rs::duplicate_codes_keep_separate_entries`。

### 方案三：旧错误 JSON 的兼容

- **A. 保持 untagged derive**：旧 JSON 可读，但集合索引对象 `{"1": {...}}`
  一律读成 `Struct`，索引类型继续丢失（只掩盖缺陷）。
- **B. 自定义 `Deserialize`（采用）**：数组 → `Field`；非空、键全部可解析为
  `usize` 且值全为对象的对象 → `List`；其余对象 → `Struct`。
  稳定结果：旧版产出的四类 JSON（字段数组、`__all__`、嵌套 struct 对象、
  索引键对象）都能读出确定的错误位置；序列化键（`__all__`、字段名、索引）
  完全不变。已知边界：若 struct 的字段名全部可解析为 `usize` 且其值全为
  对象（极端 rename 场景），会被判为 `List`——该形状与旧版 `List` 输出
  无法区分，属于可接受的取舍。回归测试：
  `validator/tests/merge.rs::old_error_json_deserializes_to_the_expected_positions`、
  `list_indices_survive_a_serialization_roundtrip`。

### 附带取舍：多层 `Vec` 的载体键

`Vec<Vec<T>>` 的内层集合没有字段名可附着，当前类型模型
（`List(BTreeMap<usize, Box<ValidationErrors>>)`）要求元素是 map，
因此内层保留 `_tmp_validator` 载体键。曾评估把元素类型改为
`ValidationErrorsKind` 以消除载体键，但那是破坏性的公共 API 变更且会改变
旧 JSON 形状，否决。多层结构的位置（外层索引 → 内层索引 → 元素错误）
保持可区分，回归测试：
`nested_schema.rs::multi_level_vec_keeps_distinguishable_positions`。

## 修改内容（集中在错误路径组合，未特判 derive）

- `validator/src/types.rs`
  - 新增内部常量 `TMP_COLLECTION_KEY` 与私有方法 `into_kind`：集合结果在
    附着到字段路径时统一剥离 `_tmp_validator` 载体键（仅当它是唯一键）。
  - `merge`：与 `merge_self` 一致地剥离载体键，集合索引直接挂在字段名下。
  - `merge_all`：新的元素提取 `into_element`——识别文档化的预包装形状
    （唯一键 `{field: Struct(..)}`）并解包一层；其余（裸 `element.validate()`
    结果）整体保留为该索引下的元素错误，不再静默丢弃。
  - `ValidationErrorsKind`：保留 untagged `Serialize`（序列化键不变），
    改为手写 `Deserialize` 以恢复 `List` 的索引类型。
- `validator/src/traits.rs`：集合 `Validate` 实现改用共享常量，行为不变。
- 未改动 derive 宏：`add`/`add_nested` 对同名字段的既有契约（字段校验优先、
  逆序 panic）维持原语义。

## 验证证据

- `cargo fetch --locked`：通过（本仓库不提交 `Cargo.lock`，已按校验流程在
  本地生成；文件被 `.gitignore` 忽略，不进交付快照）。
- `cargo +nightly test --locked --workspace --all-features`：全部通过
  （`--all-features` 启用 `proc-macro-error3/nightly`，需要 nightly 工具链，
  与上游 CI 的 `test_validator-nightly` 作业一致；该特性下 trybuild
  compile-fail 用例按 `cfg(not(feature = "nightly"))` 跳过）。
  新增回归： `validator/tests/merge.rs` 13 个、
  `validator_derive_tests/tests/nested_schema.rs` 9 个，覆盖注入场景、
  完整错误树、稳定序列化、合并顺序反转变形、多层 `Vec`、`Option<Vec<_>>`、
  空集合、自定义 code、serde rename、重复 code 与旧 JSON 兼容。
- `cargo test --locked --workspace`（stable）：除既有的
  `validator_derive_tests --test compile_test` 失败外全部通过；该失败在未修改的
  上游快照上同样复现（trybuild 的 `.stderr` 快照与 rustc 1.98.1 输出漂移），
  与本次修改无关。
- `cargo fmt --check`：通过。

## 随机源与并发控制

- 错误 map 为 `HashMap`，其迭代顺序是进程内随机源；所有序列化断言均比较
  解析后的 `serde_json::Value`（对象相等性与键序无关），不依赖字符串顺序。
- 集合索引使用 `BTreeMap`，顺序确定；测试不启动线程、不依赖调度，
  无间歇性回归风险。
