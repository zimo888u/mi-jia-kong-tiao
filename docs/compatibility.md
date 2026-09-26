# 空调兼容性

程序按米家账号返回的 model 精确匹配官方 MIoT 规格，不根据匹数或名称猜测属性地址。显示名称优先使用账号设备名（包括用户重命名）。

内置以下 46 个型号，其中 36 个采用正式发布的规格，10 个仅有预览或调试规格。它们均通过离线解析和写入范围校验，新增型号未逐台真机验证。不承诺所有 1 匹、1.5 匹空调都可控制。

- `viomi.airc.chacru`
- `viomi.airc.m1`
- `viomi.airc.m2`
- `viomi.airc.m3`
- `viomi.airc.m4`
- `viomi.airc.sd25`
- `viomi.airc.sd26`
- `viomi.airc.sd27`
- `viomi.aircondition.v6`
- `viomi.aircondition.v7`
- `viomi.aircondition.v8`
- `viomi.aircondition.v9`
- `xiaomi.airc.h10h00`
- `xiaomi.airc.h21h00`
- `xiaomi.airc.h25h00`
- `xiaomi.airc.h38h00`
- `xiaomi.airc.h39h00`
- `xiaomi.airc.h50h00`
- `xiaomi.airc.h51h00`
- `xiaomi.airc.h53h00`
- `xiaomi.airc.h54h00`
- `xiaomi.airc.h55h00`
- `xiaomi.airc.h59h00`
- `xiaomi.aircondition.ma1`
- `xiaomi.aircondition.ma2`
- `xiaomi.aircondition.ma3`
- `xiaomi.aircondition.ma4`
- `xiaomi.aircondition.ma5`
- `xiaomi.aircondition.ma6`
- `xiaomi.aircondition.ma7`
- `xiaomi.aircondition.ma8`
- `xiaomi.aircondition.ma9`
- `xiaomi.aircondition.mh1`
- `xiaomi.aircondition.mh2`
- `xiaomi.aircondition.mh3`
- `xiaomi.aircondition.mh4`
- `xiaomi.aircondition.mh5`
- `xiaomi.aircondition.mh6`
- `zhimi.aircondition.ma1`
- `zhimi.aircondition.ma2`
- `zhimi.aircondition.ma3`
- `zhimi.aircondition.ma4`
- `zhimi.aircondition.sa1`
- `zhimi.aircondition.sa10`
- `zhimi.aircondition.v1`
- `zhimi.aircondition.v2`

## 功能边界

通用适配包括开关、目标温度、模式、风速以及规格公开且可写的摆风、ECO、辅热、干燥、睡眠、灯光、提示音。不同机型功能不同；未知写入被拒绝，不会套用其他型号的地址。

`xiaomi.aircondition.ma1`、`ma2`、`ma4` 的公开版本将相同模式映射到不同数字。仅凭 model 无法区分设备版本，程序因此禁用这三个型号的模式写入。`zhimi.aircondition.ma3` 的旧版与新版温度步长不同，程序采用正式版的 0.5°C 步长，这是两版都能表示的温度档位。

在线获取的型号如有多个同优先级规格版本，也会保守禁用模式写入；其开关和温度仍按选定的规格处理，具体固件兼容性需要真机验证。

除原机型外，公开的实时电量属性未明确标注单位，界面会说明单位未确认；历史电量统计只用于原机型。预览或调试规格的实际能力需以设备固件和真机验证为准。

原机型 xiaomi.airc.h53h00 保留原有专属功能。其他型号的历史耗电、私有风感/定格和深度诊断没有通用协议保证，不作兼容承诺；不可用读数显示占位。

未内置型号通过 https://miot-spec.org/miot-spec-v2/instances?status=all 查询精确型号的最新规格，再调用 instance 接口读取公开能力。规格缓存在用户数据目录的 miot-spec-cache 中，不包含账号凭据。联网失败且没有缓存、或规格缺少安全控制所需能力时会明确报错。固件与最新版规格有差异的设备仍需实际验证。

## 更新规格

在项目根目录执行 `node scripts/update-miot-specs.mjs`，需要可访问官方规格服务的网络以及支持 fetch 的 Node.js。脚本只获取公开规格，不读取本机账号或控制设备。生成文件纳入源码，普通用户不需要 Node.js。

验证：`cargo test --workspace --locked`；离线界面：`cargo run -p miac-app --example ui-preview -- target/multi-model-preview`。
