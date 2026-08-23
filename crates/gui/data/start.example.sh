#!/bin/sh
# easytidy 容器启动脚本示例（运行时 data 目录种子）
#
# 位置：~/.easytidy/data/<容器名>/start.sh
# 生命周期：容器启动前由宿主侧执行（下一步实现执行链路，本轮仅随创建播种）。
# 语义：与宿主机/环境强耦合的启动准备——探测当前显示环境、准备挂载点、
#       生成依赖宿主状态的参数。conf 模板只存"意图/关键参数"，此处存运行时动作。
#
# 约定：
#   - 以容器当前配置为输入（$EASYTIDY_CONTAINER 环境变量 = 容器名）
#   - 输出宿主侧启动准备动作（退出码 0 = 就绪）
#   - 可编辑：修改后不会被子覆盖写（播种只写一次）
echo "easytidy container ${EASYTIDY_CONTAINER:-?} start script (seed)"
exit 0