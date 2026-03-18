#!/usr/bin/env python
# -*- coding: utf-8 -*-
content = '''---
name: xiaohongshu-poster
description: 小红书自动登录发帖助手，专注于小红书平台的自动化登录和内容发布
---

# 小红书自动登录发帖助手

## 角色定义
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

## 依赖安装

```bash
pip install playwright
playwright install chromium
```

## 完整Python代码示例

```python
"""
小红书自动发帖助手
基于 Playwright 的浏览器自动化工具
"""
import asyncio
import json
import os
import time
import random
from pathlib import Path
from datetime import datetime
from playwright.async_api import async_playwright, Playwright, Browser, Page, BrowserContext


class XiaoHongShuPoster:
    """小红书自动发帖助手类"""
    
    def __init__(self, cookies_file: str = "xiaohongshu_cookies.json"):
        self.cookies_file = cookies_file
        self.playwright: Playwright = None
        self.browser: Browser = None
        self.context: BrowserContext = None
        self.page: Page = None
        self.is_logged_in = False
        
    async def init_browser(self, headless: bool = False):
        """初始化浏览器"""
        self.playwright = await async_playwright().start()
        self.browser = await self.playwright.chromium.launch(
            headless=headless,
            args=['--disable-blink-features=AutomationControlled']
        )
        self.context = await self.browser.new_context(
            viewport={'width': 1280, 'height': 720},
            user_agent='Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'
        )
        self.page = await self.context.new_page()
        
    async def save_cookies(self):
        """保存 cookies 到文件"""
        cookies = await self.context.cookies()
        with open(self.cookies_file, 'w', encoding='utf-8') as f:
            json.dump(cookies, f, ensure_ascii=False, indent=2)
        print(f"[+] Cookies 已保存到: {self.cookies_file}")
        
    async def load_cookies(self):
        """从文件加载 cookies"""
        if os.path.exists(self.cookies_file):
            with open(self.cookies_file, 'r', encoding='utf-8') as f:
                cookies = json.load(f)
            await self.context.add_cookies(cookies)
            print(f"[+] Cookies 已加载: {self.cookies_file}")
            return True
        return False
    
    async def check_login_status(self) -> bool:
        """检查登录状态"""
        await self.page.goto("https://www.xiaohongshu.com/explore")
        await self.page.wait_for_load_state("networkidle")
        
        # 检查是否有用户头像（登录后的标志）
        try:
            avatar = await self.page.query_selector('.user-avatar, .avatar, [class*="avatar"]')
            if avatar:
                self.is_logged_in = True
                print("[+] 已检测到登录状态")
                return True
        except:
            pass
        
        self.is_logged_in = False
        print("[*] 未检测到登录状态，需要扫码登录")
        return False
    
    async def login_with_qrcode(self, timeout: int = 120) -> bool:
        """二维码扫码登录"""
        print("[*] 正在打开登录页面...")
        await self.page.goto("https://www.xiaohongshu.com/explore")
        
        # 点击登录按钮
        try:
            login_btn = await self.page.query_selector('text="登录"')
            if login_btn:
                await login_btn.click()
                await self.page.wait_for_timeout(1000)
        except Exception as e:
            print(f"[*] 点击登录按钮: {e}")
            
        # 等待二维码出现
        try:
            qrcode = await self.page.wait_for_selector('img[src*="qrcode"], img[class*="qrcode"]', timeout=10000)
            if qrcode:
                print("[+] 请在 120 秒内使用小红书 APP 扫码登录...")
                # 等待扫码成功（检测页面变化）
                await self.page.wait_for_function(
                    "() => document.querySelector('.user-avatar, .avatar, [class*=\"avatar\"]') !== null",
                    timeout=timeout * 1000
                )
                self.is_logged_in = True
                await self.save_cookies()
                print("[+] 登录成功！")
                return True
        except Exception as e:
            print(f"[-] 二维码登录失败: {e}")
            return False
            
        return False
    
    async def publish_note(self, title: str, content: str, images: list = None, 
                           tags: list = None, location: str = None) -> bool:
        """发布笔记"""
        if not self.is_logged_in:
            print("[-] 未登录，请先登录")
            return False
            
        try:
            # 进入发布页面
            print("[*] 正在进入发布页面...")
            await self.page.goto("https://creator.xiaohongshu.com/creator/post")
            await self.page.wait_for_load_state("networkidle")
            await self.page.wait_for_timeout(2000)
            
            # 点击发布按钮（如果需要）
            try:
                publish_btn = await self.page.query_selector('text="发布笔记"]')
                if publish_btn:
                    await publish_btn.click()
            except:
                pass
            
            # 上传图片（如果有）
            if images and len(images) > 0:
                print(f"[*] 正在上传 {len(images)} 张图片...")
                # 查找文件上传 input
                file_input = await self.page.query_selector('input[type="file"]')
                if file_input:
                    await file_input.set_input_files(images)
                    await self.page.wait_for_timeout(2000)
                    
            # 输入标题
            print("[*] 正在输入标题...")
            title_input = await self.page.query_selector('input[placeholder*="标题"], input[name="title"]')
            if title_input:
                await title_input.fill(title)
                await self.page.wait_for_timeout(500)
                
            # 输入正文内容
            print("[*] 正在输入正文内容...")
            # 尝试多种选择器
            content_selectors = [
                'textarea[placeholder*="正文"], textarea[name="content"]',
                'div[contenteditable="true"]',
                'div[role="textbox"]'
            ]
            content_box = None
            for selector in content_selectors:
                content_box = await self.page.query_selector(selector)
                if content_box:
                    break
                    
            if content_box:
                await content_box.click()
                await self.page.wait_for_timeout(300)
                # 逐行输入内容，模拟人工
                for line in content.split('\\n'):
                    await content_box.type(line, delay=random.randint(50, 150))
                    await self.page.keyboard.press('Enter')
                    await self.page.wait_for_timeout(200)
                    
            # 添加标签
            if tags and len(tags) > 0:
                print(f"[*] 正在添加 {len(tags)} 个标签...")
                for tag in tags:
                    # 点击话题输入框
                    tag_input = await self.page.query_selector('input[placeholder*="话题"], input[placeholder*="标签"]')
                    if tag_input:
                        await tag_input.fill(f"#{tag}")
                        await self.page.wait_for_timeout(500)
                        await self.page.keyboard.press('Enter')
                        await self.page.wait_for_timeout(300)
                        
            # 添加地点（如果需要）
            if location:
                print(f"[*] 正在添加地点: {location}")
                location_btn = await self.page.query_selector('text="添加地点"]')
                if location_btn:
                    await location_btn.click()
                    await self.page.wait_for_timeout(500)
                    location_input = await self.page.query_selector('input[placeholder*="地点"]')
                    if location_input:
                        await location_input.fill(location)
                        await self.page.wait_for_timeout(1000)
                        await self.page.keyboard.press('Enter')
                        
            # 点击发布按钮
            print("[*] 正在提交发布...")
            submit_btn = await self.page.query_selector('button:has-text("发布"), button:has-text("发布笔记")]')
            if submit_btn:
                await submit_btn.click()
                await self.page.wait_for_timeout(3000)
                
            # 确认发布
            try:
                confirm_btn = await self.page.query_selector('button:has-text("确认"), button:has-text("确定")]')
                if confirm_btn:
                    await confirm_btn.click()
                    await self.page.wait_for_timeout(2000)
            except:
                pass
                
            print("[+] 笔记发布完成！")
            return True
            
        except Exception as e:
            print(f"[-] 发布笔记失败: {e}")
            return False
            
    async def close(self):
        """关闭浏览器"""
        if self.browser:
            await self.browser.close()
        if self.playwright:
            await self.playwright.stop()
        print("[*] 浏览器已关闭")


async def main():
    """主函数示例"""
    poster = XiaoHongShuPoster(cookies_file="xiaohongshu_cookies.json")
    
    try:
        # 初始化浏览器
        print("[*] 初始化浏览器...")
        await poster.init_browser(headless=False)  # 建议使用 headless=False 方便调试
        
        # 尝试加载 cookies 登录
        if await poster.load_cookies():
            if await poster.check_login_status():
                print("[+] Cookie 登录成功！")
            else:
                print("[*] Cookie 已过期，需要重新扫码登录")
                await poster.login_with_qrcode()
        else:
            # 需要扫码登录
            print("[*] 未找到 Cookie，需要扫码登录")
            await poster.login_with_qrcode()
            
        # 检查登录状态
        if poster.is_logged_in:
            # 发布笔记示例
            success = await poster.publish_note(
                title="测试笔记标题",
                content="这是测试笔记的内容\\n\\n欢迎关注",
                images=[],  # 如需上传图片: ["path/to/image1.jpg", "path/to/image2.jpg"]
                tags=["测试", "自动化"],
                location="上海"
            )
            
            if success:
                print("[+] 笔记发布成功！")
            else:
                print("[-] 笔记发布失败")
        else:
            print("[-] 登录失败")
            
    except Exception as e:
        print(f"[-] 运行出错: {e}")
    finally:
        await poster.close()


if __name__ == "__main__":
    asyncio.run(main())
```

## 注意事项

### 1. 安全优先
- 推荐使用二维码扫码登录，避免保存明文密码
- 登录后保存 cookies，后续使用 cookies 免登录
- 妥善保管 cookies 文件，不要提交到代码仓库
- 建议将 cookies 文件加入 `.gitignore`

### 2. 遵守平台规则
- 不要频繁发送相同内容，可能被判定为垃圾内容
- 合理设置发布间隔（建议每次发布间隔 30 秒以上）
- 遵守小红书社区规范，避免违规操作
- **本工具仅供个人学习研究使用，禁止用于商业推广或批量垃圾内容发布**

### 3. 反检测建议
- 使用 `headless=False` 模式（可见浏览器）
- 适当添加随机延时模拟人工操作
- 使用真实的 User-Agent
- 避免使用过于规律的自动化操作

### 4. 合法用途声明
- 本工具仅供学习 Playwright 浏览器自动化技术
- 请确保您的使用行为符合小红书平台服务条款
- 禁止使用本工具进行账号批量注册、内容批量发布等违规操作
- 如需商业使用，请咨询平台官方获取授权
'''

target = 'D:/code/rustpilot/rustpilot/skills/xiaohongshu-poster/SKILL.md'
with open(target, 'w', encoding='utf-8') as f:
    f.write(content)
print('Done')
