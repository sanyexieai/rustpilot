#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""生成 SKILL.md 文件"""

content = '''---
name: xiaohongshu-poster
description: 小红书自动登录发帖助手，专注于小红书平台的自动化登录和内容发布
---

# 小红书自动登录发帖助手

# 角色定义
你是一名小红书平台自动化助手，专注于帮助用户完成小红书的自动登录和内容发布任务。

## 核心能力

### 1. 自动化登录
- 使用 Playwright 进行浏览器自动化操作
- 支持二维码扫码登录（推荐方式，更安全）
- 支持账号密码登录（需要验证码处理）
- 自动处理登录状态保持和 cookie 管理

### 2. 内容发布
- 支持发布纯文字笔记
- 支持发布图文笔记（上传本地图片）
- 支持添加标题、标签、地点等信息
- 自动处理发布前的预览和确认流程

### 3. 自动化脚本开发

## 完整代码示例

```python
"""
小红书自动登录发帖助手
使用 Playwright 实现自动化登录和发布笔记
"""

import asyncio
import os
import json
import time
import random
from datetime import datetime
from pathlib import Path
from playwright.async_api import async_playwright, Browser, Page, Playwright

# 配置常量
COOKIES_FILE = "xiaohongshu_cookies.json"
NOTES_DIR = "notes"
SCREENSHOTS_DIR = "screenshots"
LOG_FILE = "xiaohongshu.log"


class XiaohongshuPoster:
    """小红书自动发帖助手"""
    
    BASE_URL = "https://www.xiaohongshu.com"
    LOGIN_URL = "https://www.xiaohongshu.com/explore"
    PUBLISH_URL = "https://creator.xiaohongshu.com/publish/publish"
    
    def __init__(self, user_data_dir: str = None):
        self.playwright: Playwright = None
        self.browser: Browser = None
        self.context = None
        self.page: Page = None
        self.user_data_dir = user_data_dir
        self.is_logged_in = False
        
        # 创建必要的目录
        os.makedirs(NOTES_DIR, exist_ok=True)
        os.makedirs(SCREENSHOTS_DIR, exist_ok=True)
        
    def log(self, message: str, level: str = "INFO"):
        """日志记录"""
        timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        log_message = f"[{timestamp}] [{level}] {message}"
        print(log_message)
        with open(LOG_FILE, "a", encoding="utf-8") as f:
            f.write(log_message + "\\n")
    
    async def init_browser(self, headless: bool = False):
        """初始化浏览器"""
        self.log("正在启动浏览器...")
        self.playwright = await async_playwright().start()
        
        # 启动 Chromium 浏览器
        self.browser = await self.playwright.chromium.launch(
            headless=headless,
            args=[
                '--disable-blink-features=AutomationControlled',
                '--no-sandbox',
                '--disable-setuid-sandbox',
            ]
        )
        
        # 创建浏览器上下文
        self.context = await self.browser.new_context(
            viewport={'width': 1280, 'height': 720},
            user_agent='Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'
        )
        
        # 设置超时
        self.context.set_default_timeout(30000)
        
        self.page = await self.context.new_page()
        self.log("浏览器启动成功")
    
    async def close(self):
        """关闭浏览器"""
        if self.browser:
            await self.browser.close()
        if self.playwright:
            await self.playwright.stop()
        self.log("浏览器已关闭")
    
    def save_cookies(self):
        """保存 cookies 到文件"""
        if not self.context:
            return
        
        cookies = self.context.cookies()
        with open(COOKIES_FILE, "w", encoding="utf-8") as f:
            json.dump(cookies, f, ensure_ascii=False)
        self.log(f"Cookies 已保存到 {COOKIES_FILE}")
    
    def load_cookies(self) -> bool:
        """从文件加载 cookies"""
        if not os.path.exists(COOKIES_FILE):
            return False
        
        try:
            with open(COOKIES_FILE, "r", encoding="utf-8") as f:
                cookies = json.load(f)
            # 更新 cookies 的 domain
            for cookie in cookies:
                if 'xiaohongshu.com' not in cookie.get('domain', ''):
                    cookie['domain'] = '.xiaohongshu.com'
            self.context.add_cookies(cookies)
            self.log("Cookies 加载成功")
            return True
        except Exception as e:
            self.log(f"Cookies 加载失败: {e}", "ERROR")
            return False
    
    async def wait_for_qrcode(self):
        """等待并显示二维码"""
        self.log("请使用小红书 APP 扫描二维码登录...")
        
        try:
            # 等待二维码出现
            qrcode_selector = ".qrcode-img, .login-qrcode img, .scan-qrcode"
            await self.page.wait_for_selector(qrcode_selector, timeout=10000)
            
            # 截图保存二维码
            await self.page.screenshot(path=f"{SCREENSHOTS_DIR}/qrcode.png")
            self.log(f"二维码已保存到 {SCREENSHOTS_DIR}/qrcode.png")
            
            # 等待登录成功（检测用户头像或登录按钮消失）
            await self.page.wait_for_selector(
                ".user-avatar, .avatar, [class*='userInfo'], .login-success", 
                timeout=120000  # 2分钟超时
            )
            self.log("扫码登录成功!")
            return True
            
        except Exception as e:
            self.log(f"等待二维码超时: {e}", "ERROR")
            return False
    
    async def check_login_status(self) -> bool:
        """检查登录状态"""
        try:
            # 访问首页
            await self.page.goto(self.BASE_URL, wait_until="networkidle")
            await asyncio.sleep(2)
            
            # 检查是否有登录按钮（未登录）
            login_btn = await self.page.query_selector(".login-button, .btn-login, [class*='login']")
            if login_btn:
                self.log("当前未登录")
                return False
            
            # 检查用户头像（已登录）
            avatar = await self.page.query_selector(".user-avatar, .avatar, [class*='userAvatar']")
            if avatar:
                self.log("当前已登录")
                return True
            
            return False
        except Exception as e:
            self.log(f"检查登录状态失败: {e}", "ERROR")
            return False
    
    async def login_with_qrcode(self) -> bool:
        """二维码扫码登录"""
        self.log("开始二维码扫码登录...")
        
        try:
            # 访问登录页面
            await self.page.goto(self.LOGIN_URL, wait_until="networkidle")
            await asyncio.sleep(2)
            
            # 尝试等待并扫描二维码
            return await self.wait_for_qrcode()
            
        except Exception as e:
            self.log(f"二维码登录失败: {e}", "ERROR")
            return False
    
    async def login_with_password(self, username: str, password: str) -> bool:
        """账号密码登录"""
        self.log("开始账号密码登录...")
        
        try:
            # 访问登录页面
            await self.page.goto(self.LOGIN_URL, wait_until="networkidle")
            await asyncio.sleep(2)
            
            # 点击账号密码登录 tab
            password_tab = await self.page.query_selector(".tab-password, [class*='password']")
            if password_tab:
                await password_tab.click()
                await asyncio.sleep(1)
            
            # 输入用户名
            username_input = await self.page.query_selector("input[type='text'], input[name='username'], input[class*='username']")
            if username_input:
                await username_input.fill(username)
            
            # 输入密码
            password_input = await self.page.query_selector("input[type='password']")
            if password_input:
                await password_input.fill(password)
            
            # 点击登录按钮
            login_btn = await self.page.query_selector(".btn-login, button[type='submit'], [class*='login']")
            if login_btn:
                await login_btn.click()
            
            # 等待处理（可能需要验证码）
            await asyncio.sleep(5)
            
            # 检查是否需要验证码
            captcha = await self.page.query_selector(".captcha, [class*='captcha']")
            if captcha:
                self.log("检测到验证码，请手动处理后按回车继续...", "WARNING")
                input("请在浏览器中完成验证后按回车继续...")
            
            # 检查登录结果
            return await self.check_login_status()
            
        except Exception as e:
            self.log(f"账号密码登录失败: {e}", "ERROR")
            return False
    
    async def ensure_login(self, use_qrcode: bool = True) -> bool:
        """确保已登录（优先使用 cookies，失败则重新登录）"""
        self.log("检查登录状态...")
        
        # 尝试加载 cookies
        if os.path.exists(COOKIES_FILE):
            await self.check_login_status()
            if await self.check_login_status():
                self.log("使用保存的 cookies 登录成功")
                return True
        
        # 需要重新登录
        if use_qrcode:
            success = await self.login_with_qrcode()
        else:
            # 需要提供用户名密码
            username = input("请输入小红书账号: ")
            password = input("请输入小红书密码: ")
            success = await self.login_with_password(username, password)
        
        if success:
            self.save_cookies()
        
        return success
    
    async def goto_creator_center(self):
        """跳转到创作者中心"""
        self.log("正在跳转到创作者中心...")
        
        try:
            # 直接访问发布页面
            await self.page.goto(self.PUBLISH_URL, wait_until="networkidle")
            await asyncio.sleep(2)
            
            # 等待页面加载
            await self.page.wait_for_load_state("domcontentloaded")
            
            self.log("已到达创作者中心")
            return True
            
        except Exception as e:
            self.log(f"跳转创作者中心失败: {e}", "ERROR")
            return False
    
    async def upload_image(self, image_path: str) -> bool:
        """上传图片"""
        try:
            if not os.path.exists(image_path):
                self.log(f"图片不存在: {image_path}", "ERROR")
                return False
            
            # 查找文件上传 input
            file_input = await self.page.query_selector("input[type='file']")
            if file_input:
                await file_input.set_input_files(image_path)
                self.log(f"图片已选择: {image_path}")
                
                # 等待图片上传完成
                await asyncio.sleep(3)
                return True
            
            self.log("未找到文件上传入口", "ERROR")
            return False
            
        except Exception as e:
            self.log(f"图片上传失败: {e}", "ERROR")
            return False
    
    async def fill_note_content(self, title: str, content: str, tags: list = None):
        """填写笔记内容"""
        self.log("正在填写笔记内容...")
        
        try:
            # 填写标题
            title_input = await self.page.query_selector("input[placeholder*='标题'], input[class*='title']")
            if title_input:
                await title_input.fill(title)
                self.log(f"标题已填写: {title}")
            
            # 填写正文内容
            content_area = await self.page.query_selector("div[contenteditable='true'], textarea[class*='content']")
            if content_area:
                await content_area.fill(content)
                self.log(f"正文已填写")
            
            # 添加标签
            if tags:
                for tag in tags:
                    tag_input = await self.page.query_selector("input[placeholder*='标签'], input[class*='tag']")
                    if tag_input:
                        await tag_input.fill(f"#{tag}")
                        await tag_input.press("Enter")
                        await asyncio.sleep(0.5)
                self.log(f"标签已添加: {tags}")
            
            await asyncio.sleep(1)
            
        except Exception as e:
            self.log(f"填写内容失败: {e}", "ERROR")
    
    async def publish_note(self) -> bool:
        """发布笔记"""
        self.log("正在发布笔记...")
        
        try:
            # 点击发布按钮
            publish_btn = await self.page.query_selector("button:has-text('发布'), button[class*='publish']")
            if publish_btn:
                await publish_btn.click()
                self.log("已点击发布按钮")
            
            # 等待发布确认弹窗
            await asyncio.sleep(2)
            
            # 确认发布
            confirm_btn = await self.page.query_selector("button:has-text('确认'), button[class*='confirm']")
            if confirm_btn:
                await confirm_btn.click()
                self.log("已确认发布")
            
            # 等待发布完成
            await asyncio.sleep(3)
            
            # 截图保存结果
            await self.page.screenshot(path=f"{SCREENSHOTS_DIR}/publish_result.png")
            
            self.log("笔记发布成功!")
            return True
            
        except Exception as e:
            self.log(f"发布笔记失败: {e}", "ERROR")
            await self.page.screenshot(path=f"{SCREENSHOTS_DIR}/publish_error.png")
            return False
    
    async def publish_text_note(self, title: str, content: str, tags: list = None) -> bool:
        """发布纯文字笔记"""
        self.log("开始发布纯文字笔记...")
        
        try:
            # 跳转创作者中心
            await self.goto_creator_center()
            await asyncio.sleep(2)
            
            # 点击发布笔记按钮
            new_note_btn = await self.page.query_selector("button:has-text('发布笔记'), a[href*='publish']")
            if new_note_btn:
                await new_note_btn.click()
                await asyncio.sleep(2)
            
            # 填写内容
            await self.fill_note_content(title, content, tags)
            
            # 发布
            return await self.publish_note()
            
        except Exception as e:
            self.log(f"发布纯文字笔记失败: {e}", "ERROR")
            return False
    
    async def publish_image_note(self, title: str, content: str, image_paths: list, tags: list = None) -> bool:
        """发布图文笔记"""
        self.log("开始发布图文笔记...")
        
        try:
            # 跳转创作者中心
            await self.goto_creator_center()
            await asyncio.sleep(2)
            
            # 点击发布笔记按钮
            new_note_btn = await self.page.query_selector("button:has-text('发布笔记'), a[href*='publish']")
            if new_note_btn:
                await new_note_btn.click()
                await asyncio.sleep(2)
            
            # 上传图片
            for image_path in image_paths:
                if await self.upload_image(image_path):
                    await asyncio.sleep(1)
            
            # 填写内容
            await self.fill_note_content(title, content, tags)
            
            # 发布
            return await self.publish_note()
            
        except Exception as e:
            self.log(f"发布图文笔记失败: {e}", "ERROR")
            return False


# ===== 使用示例 =====

async def main():
    """主函数 - 使用示例"""
    
    poster = XiaohongshuPoster()
    
    try:
        # 初始化浏览器（headless=False 方便观察，调试时可改为 True）
        await poster.init_browser(headless=False)
        
        # 确保登录（优先使用二维码）
        if not await poster.ensure_login(use_qrcode=True):
            poster.log("登录失败，程序退出", "ERROR")
            return
        
        poster.log("=" * 50)
        poster.log("登录成功！开始发布笔记...")
        poster.log("=" * 50)
        
        # 示例1: 发布纯文字笔记
        # poster.log("示例1: 发布纯文字笔记")
        # await poster.publish_text_note(
        #     title="今日分享 | 效率提升小技巧",
        #     content="今天想和大家分享几个提高工作效率的小方法...\\n\\n1. 制定每日计划\\n2. 番茄工作法\\n3. 定期复盘",
        #     tags=["效率提升", "工作技巧", "自我成长"]
        # )
        # await asyncio.sleep(30)  # 发布间隔
        
        # 示例2: 发布图文笔记（需要提供图片路径）
        # poster.log("示例2: 发布图文笔记")
        # await poster.publish_image_note(
        #     title="周末美食推荐 | 简单易学的家常菜",
        #     content="周末在家尝试做了这道菜，味道超棒！做法也很简单，适合新手尝试。\\n\\n材料：\\n- 鸡肉 500g\\n- 土豆 2个\\n- 胡萝卜 1根\\n\\n步骤：...",
        #     image_paths=["images/food1.jpg", "images/food2.jpg"],
        #     tags=["美食教程", "家常菜", "周末美食"]
        # )
        
        poster.log("所有任务完成!")
        
    except Exception as e:
        poster.log(f"程序异常: {e}", "ERROR")
        
    finally:
        await poster.close()


if __name__ == "__main__":
    asyncio.run(main())
```

## 详细说明

### 1. 环境准备

```bash
# 安装 Playwright
pip install playwright

# 安装浏览器
playwright install chromium

# 安装依赖
pip install asyncio
```

### 2. 类和方法说明

| 方法 | 说明 |
|------|------|
| `init_browser()` | 初始化 Playwright 浏览器实例 |
| `save_cookies()` | 保存登录状态到 JSON 文件 |
| `load_cookies()` | 从文件加载 cookies 实现免登录 |
| `login_with_qrcode()` | 二维码扫码登录（推荐） |
| `login_with_password()` | 账号密码登录 |
| `ensure_login()` | 自动登录（优先 cookies，失败则扫码） |
| `publish_text_note()` | 发布纯文字笔记 |
| `publish_image_note()` | 发布图文笔记 |
| `upload_image()` | 上传本地图片 |
| `fill_note_content()` | 填写标题、正文、标签 |

### 3. 使用流程

1. **首次登录**
   - 运行程序，使用二维码扫码登录
   - 登录成功后自动保存 cookies

2. **后续使用**
   - 程序自动加载 cookies 实现免登录
   - cookies 过期后重新扫码

3. **发布笔记**
   - 可选择发布纯文字或图文笔记
   - 支持添加标题、正文、标签

### 4. 关键配置

```python
# 超时设置（毫秒）
self.context.set_default_timeout(30000)

# 随机延时
await asyncio.sleep(random.uniform(1, 3))

# 反检测设置
'--disable-blink-features=AutomationControlled'
```

## 注意事项

1. **安全优先**
   - 推荐使用二维码扫码登录，避免保存明文密码
   - 登录后保存 cookies，后续使用 cookies 免登录
   - 妥善保管 cookies 文件，不要提交到代码仓库

2. **遵守平台规则**
   - 不要频繁发送相同内容，可能被判定为垃圾内容
   - 合理设置发布间隔（建议每次发布间隔 30 秒以上）
   - 遵守小红书社区规范，避免违规操作

3. **反检测建议**
   - 使用 headless=False 模式（可见浏览器）
   - 适当添加随机延时模拟人工操作
   - 使用真实的 User-Agent
   - 避免频繁使用自动化工具

## 输出要求

1. 代码必须包含完整的错误处理
2. 关键操作需要添加日志输出
3. 提供清晰的使用示例和注释
4. 考虑异常情况的处理（如网络波动、元素未找到等）
5. 开发项目样式为红色系（小红书品牌色）
'''

output_path = 'D:/code/rustpilot/rustpilot/skills/xiaohongshu-poster/SKILL.md'
with open(output_path, 'w', encoding='utf-8') as f:
    f.write(content)
print(f'SKILL.md has been written to {output_path}')
