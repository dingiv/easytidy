// 主 GUI（中心化模式）：容器管理（快速启动 + 我的容器）+ 镜像管理。
//
// 容器 tab 为合并面板 ContainersPanel（flavor 模板与环境列表合一——
// 本模块的目的是让用户快速启动容器）；镜像 tab 负责拉取/清理。

import { Tabs } from 'antd';
import { ContainersPanel } from './ContainersPanel';
import { ImagesPanel } from './ImagesPanel';

export function Centralized() {
  return (
    <div className="centralized">
      <Tabs
        className="centralized-tabs"
        defaultActiveKey="containers"
        items={[
          {
            key: 'containers',
            label: '容器',
            children: <ContainersPanel />,
          },
          {
            key: 'images',
            label: '镜像',
            children: <ImagesPanel />,
          },
        ]}
      />
    </div>
  );
}
