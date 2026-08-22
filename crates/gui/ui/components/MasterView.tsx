// Master GUI 总控：容器管理 + 模板 + 镜像管理，三 tab。
//
// 容器与模板分离：容器是实例（存快照，ContainersPanel），模板是配置的
// 批量管理层（存意图，FlavorsPanel）。唯一耦合点是模板「启动」—— 经
// createRequest 触发器在本组件协调：切到容器 tab + 打开页内创建表单 +
// 预选模板。镜像 tab 负责拉取/清理。

import { useCallback, useState } from 'react';
import { Tabs } from 'antd';
import { ContainersPanel } from './ContainersPanel';
import { FlavorsPanel } from './FlavorsPanel';
import { ImagesPanel } from './ImagesPanel';

export function MasterView() {
  const [activeKey, setActiveKey] = useState('containers');
  // 模板「启动」→ 容器 tab 的创建表单触发器（token 保证连续多次启动都能触发）
  const [createRequest, setCreateRequest] = useState<{
    flavor?: string;
    token: number;
  } | null>(null);

  /** 模板「启动」：切到容器 tab 并打开创建表单预选该模板 */
  const launchFlavor = useCallback((flavor: string) => {
    setCreateRequest({ flavor, token: Date.now() });
    setActiveKey('containers');
  }, []);

  return (
    <div className="master-view">
      <Tabs
        className="master-tabs"
        activeKey={activeKey}
        onChange={setActiveKey}
        items={[
          {
            key: 'containers',
            label: '容器',
            children: (
              <ContainersPanel
                createRequest={createRequest}
                onCreateRequestConsumed={() => setCreateRequest(null)}
              />
            ),
          },
          {
            key: 'flavors',
            label: '模板',
            children: <FlavorsPanel onLaunch={launchFlavor} />,
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
