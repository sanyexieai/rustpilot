---
name: xiaohongshu-automation
description: 小红书登录并自动发帖的自动化工具，支持扫码登录、图文发布等功能
---

# 小红书自动发帖工具

基于 Playwright 的小红书自动化发帖工具，支持扫码登录、图文内容发布。

## 功能特性

- 🔐 扫码登录（支持保存登录状态）
- 📝 图文内容自动发布
- 🖼️ 支持多图片上传
- 🏷️ 自动添加话题标签
- 📍 支持添加地点

## 安装依赖

```bash
pip install playwright
playwright install chromium
```

## 使用方法

### 1. 扫码登录并保存状态

```python
from xiaohongshu import XiaohongshuPoster

poster = XiaohongshuPoster()
poster.login()  # 扫码登录，状态会自动保存
```

### 2. 发布图文笔记

```python
poster = XiaohongshuPoster()
poster.login()  # 或从已保存的状态登录

poster.publish_note(
    title="我的第一篇笔记",
    content="这是笔记的正文内容...",
    images=["/path/to/image1.jpg", "/path/to/image2.jpg"],
    topics=["日常", "生活记录"]
)
```

## 完整示例代码

```python
#!/usr/bin/env python3
"""
小红书自动发帖工具
"""
import os
import json
import time
from pathlib import Path
from typing import List, Optional
from playwright.sync_api import sync_playwright, Page, Browser


class XiaohongshuPoster:
    """小红书自动发帖器"""
    
    BASE_URL = "https://www.xiaohongshu.com"
    LOGIN_URL = "https://www.xiaohongshu.com/sign_in"
    PUBLISH_URL = "https://creator.xiaohongshu.com/publish/publish"
    
    def __init__(self, state_file: str = "xhs_state.json"):
        self.state_file = state_file
        self.browser: Optional[Browser] = None
        self.page: Optional[Page] = None
        
    def _init_browser(self) -> Page:
        """初始化浏览器"""
        playwright = sync_playwright().start()
        self.browser = playwright.chromium.launch(
            headless=False,  # 扫码登录需要可视化界面
            args=["--disable-blink-features=AutomationControlled"]
        )
        
        context = self.browser.new_context(
            viewport={"width": 1280, "height": 800},
            user_agent="Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36"
        )
        
        # 加载登录状态
        if os.path.exists(self.state_file):
            with open(self.state_file, 'r') as f:
                state = json.load(f)
                context.add_cookies(state.get('cookies', []))
                if state.get('storage'):
                    context.add_init_script(f"""
                        Object.assign(localStorage, {json.dumps(state['storage'])});
                    """)
        
        self.page = context.new_page()
        return self.page
    
    def _save_state(self):
        """保存登录状态"""
        if not self.page:
            return
            
        context = self.page.context
        cookies = context.cookies()
        storage = self.page.evaluate("() => JSON.stringify(localStorage)")
        
        state = {
            'cookies': cookies,
            'storage': json.loads(storage) if storage else {}
        }
        
        with open(self.state_file, 'w') as f:
            json.dump(state, f)
    
    def login(self, timeout: int = 120) -> bool:
        """
        扫码登录
        
        Args:
            timeout: 等待扫码的超时时间（秒）
            
        Returns:
            是否登录成功
        """
        page = self._init_browser()
        
        # 访问登录页面
        page.goto(self.LOGIN_URL)
        
        # 等待用户扫码登录
        print("请使用小红书APP扫描二维码登录...")
        
        try:
            # 等待登录成功标志（个人中心链接出现）
            page.wait_for_selector('a[href="/user/profile"]', timeout=timeout * 1000)
            print("登录成功！")
            
            # 保存登录状态
            self._save_state()
            return True
            
        except Exception as e:
            print(f"登录超时或失败: {e}")
            return False
    
    def is_logged_in(self) -> bool:
        """检查是否已登录"""
        if not self.page:
            self._init_browser()
        
        self.page.goto(self.BASE_URL)
        time.sleep(2)
        
        # 检查是否存在登录按钮
        login_btn = self.page.query_selector('.login-btn, .sign-in-btn')
        return login_btn is None
    
    def publish_note(
        self,
        title: str,
        content: str,
        images: List[str],
        topics: Optional[List[str]] = None,
        location: Optional[str] = None
    ) -> bool:
        """
        发布图文笔记
        
        Args:
            title: 笔记标题
            content: 笔记正文
            images: 图片路径列表
            topics: 话题标签列表
            location: 地点名称
            
        Returns:
            是否发布成功
        """
        if not self.page:
            self._init_browser()
        
        try:
            # 进入发布页面
            self.page.goto(self.PUBLISH_URL)
            time.sleep(3)
            
            # 上传图片
            self._upload_images(images)
            
            # 填写标题
            title_input = self.page.wait_for_selector('input[placeholder*="标题"], textarea[placeholder*="标题"]')
            title_input.fill(title)
            
            # 填写正文
            content_input = self.page.wait_for_selector('div[contenteditable="true"], textarea[placeholder*="正文"]')
            content_input.fill(content)
            
            # 添加话题
            if topics:
                self._add_topics(topics)
            
            # 添加地点
            if location:
                self._add_location(location)
            
            # 点击发布
            publish_btn = self.page.wait_for_selector('button:has-text("发布"), .publish-btn')
            publish_btn.click()
            
            # 等待发布成功提示
            self.page.wait_for_selector('.success-msg, .publish-success', timeout=30000)
            print("笔记发布成功！")
            return True
            
        except Exception as e:
            print(f"发布失败: {e}")
            return False
    
    def _upload_images(self, images: List[str]):
        """上传图片"""
        # 找到文件上传输入框
        file_input = self.page.wait_for_selector('input[type="file"][accept*="image"]')
        
        # 上传所有图片
        for img_path in images:
            if not os.path.exists(img_path):
                print(f"警告: 图片不存在 {img_path}")
                continue
            file_input.set_input_files(img_path)
            time.sleep(1)  # 等待上传完成
        
        time.sleep(2)  # 等待所有图片处理完成
    
    def _add_topics(self, topics: List[str]):
        """添加话题标签"""
        for topic in topics:
            # 点击添加话题按钮
            topic_btn = self.page.query_selector('button:has-text("#话题"), .add-topic-btn')
            if topic_btn:
                topic_btn.click()
                time.sleep(1)
            
            # 输入话题
            topic_input = self.page.wait_for_selector('input[placeholder*="话题"], .topic-search input')
            topic_input.fill(f"#{topic}")
            time.sleep(1)
            
            # 选择第一个匹配的话题
            first_topic = self.page.query_selector('.topic-item, .topic-suggestion')
            if first_topic:
                first_topic.click()
            
            time.sleep(0.5)
    
    def _add_location(self, location: str):
        """添加地点"""
        # 点击添加地点按钮
        location_btn = self.page.query_selector('button:has-text("添加地点"), .add-location-btn')
        if location_btn:
            location_btn.click()
            time.sleep(1)
            
            # 搜索地点
            search_input = self.page.wait_for_selector('input[placeholder*="搜索地点"]')
            search_input.fill(location)
            time.sleep(2)
            
            # 选择第一个匹配的地点
            first_location = self.page.query_selector('.location-item')
            if first_location:
                first_location.click()
    
    def close(self):
        """关闭浏览器"""
        if self.browser:
            self.browser.close()


# 使用示例
if __name__ == "__main__":
    poster = XiaohongshuPoster()
    
    # 登录
    if not poster.is_logged_in():
        poster.login()
    
    # 发布笔记
    poster.publish_note(
        title="今日份美好 ☀️",
        content="今天天气真好，出门走走心情也变好了~\n\n大家周末都去哪里玩了呢？",
        images=["./photos/photo1.jpg", "./photos/photo2.jpg"],
        topics=["日常", "周末去哪儿", "生活记录"],
        location="上海"
    )
    
    poster.close()
```

## 注意事项

1. **登录状态**: 首次使用需要扫码登录，登录状态会保存到 `xhs_state.json` 文件
2. **频率限制**: 避免频繁发布，建议间隔5-10分钟以上
3. **图片要求**: 支持 JPG、PNG 格式，单张不超过 20MB
4. **内容规范**: 遵守小红书社区规范，避免违规内容

## 常见问题

### Q: 登录状态失效怎么办？
删除 `xhs_state.json` 文件，重新扫码登录。

### Q: 如何无头模式运行？
将 `headless=False` 改为 `headless=True`，但首次登录建议开启可视化界面。

### Q: 发布失败如何处理？
检查网络连接、登录状态，以及图片路径是否正确。
