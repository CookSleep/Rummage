"""自制长截屏：手机 / 网页竖向长截图，考查长图切片。

- 页面文本全部自行撰写（`long_pages.py`），用 HTML + CSS 仿聊天、公众号文章、商品详情、账单与订单、设置、
  新闻、菜谱、攻略、终端与代码、表格、朋友圈等真实界面，用本机 Edge 无头模式截整页 PNG。只用 Python 标准库。
- 每页先放开头的固定块，再按顺序填充页面自己的块与通用填充块（填充顺序由固定种子决定），页面填到目标高度的
  `at` 处时插入可查询的细节；填满目标高度后停止。布局在浏览器里完成：第一遍 `--dump-dom` 取整页高度与各细节的
  纵向像素区间，第二遍以整页高度为窗口高度截图，所以截到的就是整页。页面宽度固定，与窗口宽度无关。
- 宽 1080（手机）或 1280（网页）；长页面的长宽比约 2.6–15，其中若干张超过 12 段切片上限；
  另有 10 张 2.0–2.5 的普通截图作干扰项（不切片，没有查询）。
- 查询为大模型撰写的口语化中文，多数指向页面中下部的细节，少数指向整页主题。细节查询的相关项带
  `region`（细节在图中的纵向像素区间 `[上, 下]`），以后做片段级指标用。
- 文件按固定种子打乱后编号命名（`long_001.png` 起），不含描述文字。
- 构建是确定的：同样的脚本与系统字体得到同样的图（Edge 版本不同时字形渲染可能有细微差别）。
- Edge 路径可用环境变量 `EDGE` 指定。
"""

import html
import json
import os
import re
import shutil
import subprocess
import tempfile
import zlib
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import long_pages as L
from common import reset_dir, rng

NAME = 'screenshot_long'
EDGE = os.environ.get('EDGE', r'C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe')
PALETTE = ['#E57373', '#F06292', '#BA68C8', '#9575CD', '#7986CB', '#64B5F6', '#4FC3F7', '#4DB6AC',
           '#81C784', '#AED581', '#FFB74D', '#FF8A65', '#A1887F', '#90A4AE', '#4DD0E1', '#DCE775']
N_FILL = 700


def esc(s):
    return html.escape(str(s))


def color(s):
    return PALETTE[zlib.crc32(str(s).encode()) % len(PALETTE)]


def avatar(name, cls='av'):
    return f'<div class="{cls}" style="background:{color(name)}">{esc(name[:1])}</div>'


def picture(seed, cls='pic'):
    """示意配图：渐变天空、太阳和两座山，颜色由 seed 决定"""
    h = zlib.crc32(str(seed).encode())
    a, b, c = PALETTE[h % 16], PALETTE[(h >> 4) % 16], PALETTE[(h >> 8) % 16]
    sx = 20 + (h >> 12) % 60
    return (f'<div class="{cls}"><svg viewBox="0 0 100 60" preserveAspectRatio="none" width="100%" height="100%">'
            f'<defs><linearGradient id="g{h}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{a}"/>'
            f'<stop offset="1" stop-color="#fff"/></linearGradient></defs><rect width="100" height="60" fill="url(#g{h})"/>'
            f'<circle cx="{sx}" cy="18" r="7" fill="#FFF59D"/><polygon points="0,60 30,25 60,60" fill="{b}"/>'
            f'<polygon points="35,60 70,20 100,50 100,60" fill="{c}"/></svg></div>')


STATUS = ('<div class="status"><span>9:41</span><span class="si"><svg width="60" height="36" viewBox="0 0 30 18">'
          '<rect x="0" y="12" width="5" height="6"/><rect x="8" y="8" width="5" height="10"/><rect x="16" y="4" width="5" height="14"/>'
          '<rect x="24" y="0" width="5" height="18"/></svg><svg width="96" height="40" viewBox="0 0 48 20"><rect x="1" y="2" width="40" height="16" rx="4" '
          'fill="none" stroke="#000" stroke-width="2"/><rect x="4" y="5" width="28" height="10" rx="2"/><rect x="43" y="7" width="3" height="6"/></svg></span></div>')


def block(b, page):
    """把一个块渲染成 HTML。块的第一项是类型，其余字段见 long_pages.py"""
    t = b[0]
    if t == 'm':
        me = b[1] == '我'
        name = '' if me or page.get('style') != 'qq' else f'<div class="nm">{esc(b[1])}</div>'
        return (f'<div class="msg{" me" if me else ""}">{avatar(b[1])}<div class="mb">{name}'
                f'<div class="bub">{esc(b[2])}</div></div></div>')
    if t == 'pic':
        me = b[1] == '我'
        return f'<div class="msg{" me" if me else ""}">{avatar(b[1])}<div class="mb">{picture(b[2], "mpic")}</div></div>'
    if t == 'voice':
        me = b[1] == '我'
        return (f'<div class="msg{" me" if me else ""}">{avatar(b[1])}<div class="mb"><div class="bub voice" '
                f'style="width:{160 + b[2] * 12}px">((·  {b[2]}″</div></div></div>')
    if t == 't':
        return f'<div class="time">{esc(b[1])}</div>'
    if t == 'title':
        return f'<div class="atitle"><h1>{esc(b[1])}</h1><div class="meta"><span class="acc">{esc(b[2])}</span>　{esc(b[3])}</div></div>'
    if t == 'h':
        return f'<h2>{esc(b[1])}</h2>'
    if t == 'p':
        return f'<p>{esc(b[1])}</p>'
    if t == 'q':
        return f'<blockquote>{esc(b[1])}</blockquote>'
    if t == 'img':
        return f'<figure>{picture(b[1])}<figcaption>{esc(b[1])}</figcaption></figure>'
    if t == 'ptop':
        tags = ''.join(f'<span class="tag">{esc(x)}</span>' for x in b[3])
        return (f'<div class="ptop"><div class="hero" style="background:linear-gradient(160deg,#fafafa,#e0e0e0)">'
                f'<div class="prod" style="background:{b[4]}"></div></div><div class="card"><div class="price">¥<b>{esc(b[1])}</b>'
                f'<span class="sold">券后价</span></div><div class="ptitle">{esc(b[2])}</div><div>{tags}</div></div></div>')
    if t == 'banner':
        return (f'<div class="banner" style="background:{color(b[1])}"><div class="bt">{esc(b[1])}</div>'
                f'<div class="bs">{esc(b[2])}</div>{picture(b[1], "bpic")}</div>')
    if t == 'spec':
        rows = ''.join(f'<tr><td class="k">{esc(k)}</td><td>{esc(v)}</td></tr>' for k, v in b[2])
        return f'<div class="card"><div class="ct">{esc(b[1])}</div><table class="spec">{rows}</table></div>'
    if t == 'review':
        return (f'<div class="card review"><div class="rh">{avatar(b[1], "rav")}<span>{esc(b[1])}</span>'
                f'<span class="stars">★★★★★</span></div><div class="rt">{esc(b[2])}</div></div>')
    if t == 'summary':
        return (f'<div class="sum"><div class="sm">{esc(b[1])}</div><div class="sr"><div>支出<b>¥{esc(b[2])}</b></div>'
                f'<div>收入<b>¥{esc(b[3])}</b></div></div></div>')
    if t == 'day':
        if page['kind'] == 'travel':
            return f'<div class="dayb">{esc(b[1])}</div>'
        return f'<div class="day"><span>{esc(b[1])}</span><span>{esc(b[2])}</span></div>'
    if t == 'tx':
        cls = 'plus' if b[3].startswith('+') else ''
        return (f'<div class="tx"><div class="ic" style="background:{color(b[2])}">{esc(b[2][:1])}</div><div class="tm">'
                f'<div class="tn">{esc(b[1])}</div><div class="tt">{esc(b[4])}</div></div><div class="ta {cls}">{esc(b[3])}</div></div>')
    if t == 'otabs':
        return '<div class="otabs">' + ''.join(f'<span{" class=on" if i == 0 else ""}>{esc(x)}</span>' for i, x in enumerate(b[1])) + '</div>'
    if t == 'order':
        return (f'<div class="card order"><div class="oh"><span class="shop">▣ {esc(b[1])}　›</span><span class="st">{esc(b[4])}</span></div>'
                f'<div class="ob">{picture(b[2], "thumb")}<div class="oi">{esc(b[2])}</div><div class="op">{esc(b[3])}</div></div>'
                f'<div class="obtn"><span>查看物流</span><span>再次购买</span></div></div>')
    if t == 'g':
        return f'<div class="grp">{esc(b[1])}</div>'
    if t == 'row':
        return f'<div class="row"><span class="rl">{esc(b[1])}</span><span class="rv">{esc(b[2])}　›</span></div>'
    if t == 'sw':
        return (f'<div class="row sw"><div><div class="rl">{esc(b[1])}</div><div class="rd">{esc(b[2])}</div></div>'
                f'<span class="tg{" on" if b[3] else ""}"><i></i></span></div>')
    if t == 'ntabs':
        return '<div class="ntabs">' + ''.join(f'<span{" class=on" if x == b[2] else ""}>{esc(x)}</span>' for x in b[1]) + '</div>'
    if t == 'n':
        img = picture(b[1], 'nimg') if b[4] else ''
        return (f'<div class="news"><div class="nt"><div class="nh">{esc(b[1])}</div><div class="nm">{esc(b[2])}　'
                f'{b[3]} 评论</div></div>{img}</div>')
    if t == 'hero':
        return (f'<div class="rhero" style="background:{b[3]}">{picture(b[1], "rpic")}<div class="rht"><h1>{esc(b[1])}</h1>'
                f'<div>{esc(b[2])}</div></div></div>')
    if t == 'ing':
        rows = ''.join(f'<div class="ingr"><span>{esc(k)}</span><span>{esc(v)}</span></div>' for k, v in b[1])
        return f'<div class="sec"><h2>用料</h2>{rows}</div>'
    if t == 'step':
        img = picture(b[1], 'spic') if b[2] else ''
        return f'<div class="step"><div class="sn">步骤 <span class="num"></span></div><p>{esc(b[1])}</p>{img}</div>'
    if t == 'tip':
        return f'<div class="tip"><b>小贴士</b>{esc(b[1])}</div>'
    if t == 'c':
        return f'<div class="cmt">{avatar(b[1], "cav")}<div><div class="cn">{esc(b[1])}</div><div>{esc(b[2])}</div></div></div>'
    if t == 'spot':
        return (f'<div class="spot"><div class="pin"><svg width="44" height="56" viewBox="0 0 22 28"><path d="M11 0C5 0 0 5 0 11c0 8 11 17 11 17s11-9 11-17C22 5 17 0 11 0z" '
                f'fill="#E53935"/><circle cx="11" cy="11" r="4" fill="#fff"/></svg></div><div><div class="sp">{esc(b[1])}</div><p>{esc(b[2])}</p></div></div>')
    if t == 'cmd':
        return f'<div class="ln"><span class="pr">dev@build-01:~/project$</span> {esc(b[1])}</div>'
    if t == 'o':
        return f'<div class="ln {b[2]}">{esc(b[1])}</div>'
    if t == 'code':
        return f'<div class="code">{esc(b[1])}</div>'
    if t == 'tr':
        cells = ''.join(f'<td>{esc(x)}</td>' for x in b[1][:-1])
        last = b[1][-1]
        bad = last in ('缺货预警', '延误', '取消') or str(last).startswith('-')
        return f'<table class="tb"><tr>{cells}<td><span class="badge{" bad" if bad else ""}">{esc(last)}</span></td></tr></table>'
    if t == 'post':
        imgs = ''.join(picture(f'{b[2]}{i}', 'gimg') for i in range(b[3]))
        grid = f'<div class="grid g{min(b[3], 3)}">{imgs}</div>' if b[3] else ''
        likes = f'<div class="lk">♡ {esc("，".join(b[5]))}</div>' if b[5] else ''
        cmts = ''.join(f'<div class="cm"><b>{esc(n)}</b>：{esc(c)}</div>' for n, c in zip(L.NAMES, b[6]))
        box = f'<div class="lbox">{likes}{cmts}</div>' if likes or cmts else ''
        return (f'<div class="post">{avatar(b[1], "pav")}<div class="pb"><div class="pn">{esc(b[1])}</div><div class="pt">{esc(b[2])}</div>'
                f'{grid}<div class="pm"><span>{esc(b[4])}</span><span class="dots">··</span></div>{box}</div></div>')
    raise ValueError(f'未知块类型：{t}')


def fill(page, r):
    """生成通用填充块（数量足够填满任何目标高度，多余的不用）"""
    k = page['kind']
    out = list(page.get('fill', []))
    if k == 'chat':
        people = page['people'] + ['我']
        hh, mm = r.randint(8, 20), r.randint(0, 59)
        while len(out) < N_FILL:
            if r.random() < 0.13:
                mm += r.randint(3, 50)
                hh, mm = (hh + mm // 60) % 24, mm % 60
                out.append(('t', f'{hh:02d}:{mm:02d}'))
            who = r.choice(people)
            x = r.random()
            out.append(('pic', who, r.random()) if x < 0.06 else ('voice', who, r.randint(2, 30)) if x < 0.12 else ('m', who, r.choice(L.CHAT)))
    elif k == 'article':
        while len(out) < N_FILL:
            x = r.random()
            out.append(('img', r.choice(L.CAPTION)) if x < 0.12 else ('h', r.choice(L.ARTICLE_H)) if x < 0.2
                       else ('q', r.choice(L.ARTICLE_P)) if x < 0.25 else ('p', r.choice(L.ARTICLE_P)))
    elif k == 'product':
        while len(out) < N_FILL:
            x = r.random()
            out.append(('banner', *r.choice(L.PRODUCT_FEATURE)) if x < 0.35 else ('review', *r.choice(L.PRODUCT_REVIEW)) if x < 0.7
                       else ('p', r.choice(L.PRODUCT_P)))
    elif k == 'bill':
        day = 30 if page['month'] != 2 else 28
        while len(out) < N_FILL:
            n = r.randint(2, 6)
            label = f'{page["month"]}月{day}日' if not page.get('card') else f'{page["month"]:02d}-{day:02d}'
            out.append(('day', label, f'支出 ¥{r.randint(20, 900)}.{r.randint(0, 99):02d}'))
            for _ in range(n):
                name, cat, lo, hi = r.choice(L.BILL_IN if r.random() < 0.08 else L.BILL_TX)
                amt = r.uniform(lo, hi)
                sign = '+' if cat == '收入' else '-'
                t = f'{r.randint(7, 23):02d}:{r.randint(0, 59):02d}'
                out.append(('tx', name, cat, f'{sign}{amt:,.2f}', t))
            day = day - 1 if day > 1 else 30
    elif k == 'orders':
        pool = L.FOOD_ORDERS if page['pool'] == 'food' else L.ORDERS
        while len(out) < N_FILL:
            out.append(('order', *r.choice(pool)))
    elif k == 'settings':
        while len(out) < N_FILL:
            g, rows = r.choice(L.SETTINGS)
            out.append(('g', g))
            for lab, v in rows:
                out.append(('row', lab, v))
            for lab, d in r.sample(L.SETTINGS_SW, r.randint(0, 3)):
                out.append(('sw', lab, d, r.random() < 0.6))
    elif k == 'news':
        while len(out) < N_FILL:
            out.append(('n', r.choice(L.NEWS), r.choice(L.NEWS_SRC), r.randint(3, 3000), r.random() < 0.45))
    elif k == 'recipe':
        while len(out) < N_FILL:
            out.append(('tip', r.choice(L.RECIPE_TIP)) if r.random() < 0.1 else ('c', *r.choice(L.RECIPE_C)))
    elif k == 'travel':
        while len(out) < N_FILL:
            out.append(('img', r.choice(L.CAPTION)) if r.random() < 0.25 else ('tip', r.choice(L.TRAVEL_TIP)))
    elif k == 'term':
        out = term_fill(page['mode'], r)
    elif k == 'code':
        while len(out) < N_FILL:
            out.append(('code', r.choice(CODE)))
    elif k == 'table':
        while len(out) < N_FILL:
            out.append(('tr', TABLES[page['gen']](r)))
    elif k == 'moments':
        while len(out) < N_FILL:
            name, text, n = r.choice(L.POSTS)
            out.append(('post', name, text, n, f'{r.randint(1, 23)}小时前', r.sample(L.NAMES, r.randint(0, 4)),
                        r.sample(L.POST_COMMENT, r.randint(0, 2))))
    return out


MODULES = ['billing', 'orders', 'users', 'auth', 'cart', 'coupon', 'invoice', 'report', 'search', 'notify']
FUNCS = ['create_ok', 'parse_amount', 'handles_empty', 'round_trip', 'rejects_invalid', 'pagination', 'sorts_by_date',
         'merges_duplicates', 'expired_token', 'retry_backoff', 'currency_format', 'timezone_offset']
CODE = [
    'import json\nfrom datetime import datetime, timedelta\nfrom pathlib import Path',
    'def load_trips(path):\n    """读取行程文件"""\n    with open(path, encoding="utf-8") as f:\n        return json.load(f)',
    'def days_between(start, end):\n    a = datetime.strptime(start, "%Y-%m-%d")\n    b = datetime.strptime(end, "%Y-%m-%d")\n    return (b - a).days + 1',
    'class Trip:\n    def __init__(self, name, cities):\n        self.name = name\n        self.cities = list(cities)\n\n    def __repr__(self):\n        return f"Trip({self.name!r}, {len(self.cities)} cities)"',
    'def total_cost(items):\n    # 按币种汇总花费\n    out = {}\n    for it in items:\n        out[it["currency"]] = out.get(it["currency"], 0) + it["amount"]\n    return out',
    'def format_date(d):\n    return d.strftime("%m月%d日")',
    'def save(trip, path):\n    Path(path).write_text(json.dumps(trip.__dict__, ensure_ascii=False, indent=2), encoding="utf-8")',
    'if __name__ == "__main__":\n    trips = load_trips("trips.json")\n    for t in trips:\n        print(t["name"], days_between(t["start"], t["end"]))',
]


def term_fill(mode, r):
    out = []
    if mode == 'test':
        out.append(('cmd', 'cargo test -p billing'))
        out.append(('o', '   Compiling billing v0.3.2 (/home/dev/project/billing)\n    Finished test profile [unoptimized + debuginfo] target(s) in 14.82s', ''))
        while len(out) < N_FILL:
            out.append(('o', f'test {r.choice(MODULES)}::tests::{r.choice(FUNCS)}_{r.randint(1, 99)} ... ok', 'ok'))
    elif mode == 'build':
        out.append(('cmd', 'npm run build'))
        out.append(('o', '> shop-web@2.4.0 build\n> vite build', ''))
        while len(out) < N_FILL:
            name = f'{r.choice(["index", "vendor", "Checkout", "Cart", "Profile", "Search", "Order"])}-{r.getrandbits(32):08x}'
            if r.random() < 0.15:
                out.append(('o', f'[plugin:vite:esbuild] warning: unused variable "tmp{r.randint(1, 9)}" in src/{name[:5]}.tsx', 'warn'))
            else:
                ext = r.choice(['js', 'css', 'js'])
                out.append(('o', f'dist/assets/{name}.{ext}   {r.uniform(0.5, 480):8.2f} kB │ gzip: {r.uniform(0.3, 150):7.2f} kB', ''))
    else:
        sec = 3 * 3600
        while len(out) < N_FILL:
            sec += r.randint(0, 9)
            lvl = 'WARN ' if r.random() < 0.08 else 'INFO '
            mod = r.choice(['http', 'order', 'pay', 'cache', 'mq', 'auth'])
            msg = r.choice([f'GET /api/v1/orders/{r.randint(10000, 99999)} 200 {r.randint(3, 180)}ms',
                            f'POST /api/v1/pay/callback 200 {r.randint(5, 90)}ms', f'cache hit ratio {r.uniform(0.85, 0.99):.3f}',
                            f'consumed {r.randint(1, 60)} messages from topic order-events', f'token refreshed for uid={r.randint(100000, 999999)}',
                            f'slow query {r.randint(500, 1500)}ms: SELECT * FROM orders WHERE ...'])
            out.append(('o', f'2026-09-21 {sec // 3600:02d}:{sec // 60 % 60:02d}:{sec % 60:02d} {lvl} [{mod}] {msg}',
                        'warn' if lvl.startswith('WARN') else ''))
    return out


SURNAMES = '王李张刘陈杨黄赵吴周徐孙马朱胡郭何林罗高'
GIVEN = ['子轩', '雨涵', '浩然', '欣怡', '梓萱', '宇航', '思远', '佳琪', '一诺', '俊杰', '诗涵', '明轩', '晨曦', '可馨']
CITIES = ['北京', '上海', '广州', '深圳', '成都', '重庆', '武汉', '西安', '南京', '杭州', '长沙', '昆明', '厦门', '青岛']
TABLES = {
    'inventory': lambda r: [f'{r.choice(["HB", "KT", "SP", "YJ"])}-{r.randint(1000, 9999)}',
                            r.choice(['保温杯 500ml', '帆布收纳箱', '速干毛巾', '登山杖 碳素', '野餐垫 防潮', '头灯 USB 充电', '户外水壶', '睡袋 春秋款']),
                            r.choice(['北京一号仓', '上海三号仓', '广州仓', '武汉仓']), str(r.randint(60, 900)), str(r.choice([20, 30, 50, 100])), '正常'],
    'grades': lambda r: [str(2023000 + r.randint(1, 60)), r.choice(SURNAMES) + r.choice(GIVEN)] + [str(r.randint(70, 140)) for _ in range(3)]
                        + [str(r.randint(50, 99)) for _ in range(2)] + [str(r.randint(380, 590))],
    'flights': lambda r: [f'{r.choice(["MU", "CZ", "CA", "ZH", "3U"])}{r.randint(1000, 9999)}', *r.sample(CITIES, 2),
                          f'{r.randint(6, 22):02d}:{r.choice(["00", "15", "30", "45"])}', f'{r.randint(6, 22):02d}:{r.randint(0, 59):02d}',
                          r.choice(['已起飞', '已到达', '计划', '已到达'])],
    'points': lambda r: [f'2026-{r.randint(1, 9):02d}-{r.randint(1, 28):02d}', r.choice(['滨江店', '万象城店', '西湖店', '高新店', '线上商城']),
                         r.choice(['消费积分', '签到奖励', '评价奖励', '生日月加倍']), f'+{r.randint(5, 400)}', f'{r.randint(1000, 15000):,}'],
}

CSS = '''
*{box-sizing:border-box}html,body{margin:0;padding:0;overflow:hidden}
#page{width:WIDTHpx;font-family:"Microsoft YaHei","微软雅黑",sans-serif;color:#191919;background:#fff}
.status{height:110px;display:flex;justify-content:space-between;align-items:center;padding:0 60px;font-size:42px;font-weight:bold}
.si{display:flex;gap:20px;align-items:center}
.bar{height:150px;display:flex;align-items:center;justify-content:center;font-size:52px;font-weight:bold;position:relative;border-bottom:1px solid #ddd}
.bar .back{position:absolute;left:40px;font-size:70px;font-weight:normal}.bar .more{position:absolute;right:50px;font-size:60px}
.end{padding:60px;text-align:center;color:#999;font-size:36px}
/* 聊天 */
.k-chat #c{padding:20px 0 40px}.k-chat #page,.k-chat .bar{background:#EDEDED}
.msg{display:flex;padding:22px 36px;align-items:flex-start}.msg.me{flex-direction:row-reverse}
.av{width:120px;height:120px;border-radius:14px;flex:none;color:#fff;font-size:52px;display:flex;align-items:center;justify-content:center}
.mb{margin:0 26px;max-width:700px}.msg.me .mb{display:flex;flex-direction:column;align-items:flex-end}
.bub{background:#fff;border-radius:14px;padding:28px 34px;font-size:48px;line-height:1.45}.msg.me .bub{background:#95EC69}
.nm{font-size:34px;color:#888;margin:0 0 10px 6px}.voice{color:#333}
.mpic{width:420px;height:520px;border-radius:14px;overflow:hidden}
.time{text-align:center;color:#999;font-size:36px;padding:26px 0}
.s-qq #page,.s-qq .bar{background:#F2F4F8}.s-qq .av{border-radius:50%}.s-qq .bub{border-radius:36px}.s-qq .msg.me .bub{background:#2D8CF0;color:#fff}
.inputbar{height:170px;background:#F7F7F7;border-top:1px solid #ddd;display:flex;align-items:center;padding:0 30px;gap:24px}
.inputbar .in{flex:1;height:100px;background:#fff;border-radius:12px}.inputbar .ci{width:80px;height:80px;border:5px solid #333;border-radius:50%}
/* 文章 */
.k-article #c,.k-travel #c{padding:20px 64px 40px}
.atitle h1{font-size:66px;line-height:1.4;margin:30px 0 24px}.meta{font-size:38px;color:#999;margin-bottom:40px}.acc{color:#576B95}
.k-article p,.k-travel p{font-size:48px;line-height:1.85;color:#333;margin:0 0 46px;text-align:justify}
h2{font-size:54px;margin:60px 0 36px;border-left:12px solid #07C160;padding-left:24px}
blockquote{margin:0 0 46px;padding:30px 40px;background:#F6F6F6;border-left:8px solid #ccc;font-size:44px;line-height:1.7;color:#666}
figure{margin:0 0 46px}.pic{height:560px;border-radius:12px;overflow:hidden}figcaption{text-align:center;color:#999;font-size:34px;margin-top:16px}
.afoot{padding:40px 64px;font-size:38px;color:#999;display:flex;justify-content:space-between;border-top:1px solid #eee}
/* 商品 */
.k-product #page{background:#F2F2F2}.hero{height:1080px;display:flex;align-items:center;justify-content:center}
.prod{width:420px;height:640px;border-radius:60px;box-shadow:0 30px 60px rgba(0,0,0,.25)}
.card{background:#fff;border-radius:28px;margin:24px;padding:40px}
.price{color:#FF2D2D;font-size:50px}.price b{font-size:96px}.sold{font-size:34px;color:#fff;background:#FF2D2D;border-radius:8px;padding:4px 14px;margin-left:20px}
.ptitle{font-size:50px;font-weight:bold;line-height:1.45;margin:24px 0}
.tag{display:inline-block;font-size:34px;color:#FF5A00;border:2px solid #FFB08A;border-radius:8px;padding:4px 14px;margin-right:16px}
.ct{font-size:46px;font-weight:bold;margin-bottom:24px}.spec{width:100%;border-collapse:collapse;font-size:42px}
.spec td{padding:22px 0;border-bottom:1px solid #eee}.spec .k{color:#999;width:300px}
.banner{margin:24px;border-radius:28px;padding:60px;color:#fff;text-align:center}.bt{font-size:72px;font-weight:bold}.bs{font-size:44px;margin:20px 0 40px}
.bpic{height:500px;border-radius:20px;overflow:hidden}
.k-product p{font-size:42px;line-height:1.7;color:#555;background:#fff;margin:24px;padding:40px;border-radius:28px}
.rh{display:flex;align-items:center;gap:20px;font-size:40px;color:#666}.rav{width:80px;height:80px;border-radius:50%;color:#fff;display:flex;align-items:center;justify-content:center;font-size:36px}
.stars{color:#FF9500;margin-left:auto}.rt{font-size:44px;line-height:1.6;margin-top:20px}
.buybar{height:170px;background:#fff;display:flex;align-items:center;justify-content:flex-end;padding:0 30px;gap:0;border-top:1px solid #ddd}
.buybar span{height:110px;width:300px;display:flex;align-items:center;justify-content:center;color:#fff;font-size:44px}
.buybar .a{background:#FFB300;border-radius:55px 0 0 55px}.buybar .b{background:#FF3D00;border-radius:0 55px 55px 0}
/* 账单 */
.k-bill #page,.k-orders #page{background:#F5F5F5}.sum{margin:30px;padding:50px;border-radius:28px;background:#1677FF;color:#fff}
.sm{font-size:46px}.sr{display:flex;gap:80px;font-size:38px;margin-top:30px}.sr b{display:block;font-size:64px;margin-top:10px}
.day{display:flex;justify-content:space-between;padding:36px 50px 16px;font-size:38px;color:#888}
.tx{display:flex;align-items:center;background:#fff;padding:36px 50px;border-bottom:1px solid #f0f0f0}
.ic{width:100px;height:100px;border-radius:50%;color:#fff;font-size:44px;display:flex;align-items:center;justify-content:center;flex:none}
.tm{flex:1;margin-left:36px}.tn{font-size:46px}.tt{font-size:36px;color:#999;margin-top:10px}.ta{font-size:50px;font-weight:bold}.ta.plus{color:#E8710A}
.otabs,.ntabs{display:flex;justify-content:space-around;background:#fff;font-size:44px;padding:30px 0;color:#666}
.otabs .on,.ntabs .on{color:#000;font-weight:bold;border-bottom:8px solid #FF5000;padding-bottom:12px}
.oh{display:flex;justify-content:space-between;font-size:42px}.st{color:#FF5000}.ob{display:flex;gap:30px;margin:30px 0;font-size:44px;line-height:1.5}
.thumb{width:220px;height:220px;border-radius:16px;overflow:hidden;flex:none}.oi{flex:1}.op{font-weight:bold}
.obtn{display:flex;justify-content:flex-end;gap:24px}.obtn span{border:2px solid #ccc;border-radius:40px;padding:14px 36px;font-size:38px}
/* 设置 */
.k-settings #page{background:#F2F2F7}.grp{font-size:38px;color:#888;padding:50px 60px 18px}
.row{display:flex;justify-content:space-between;align-items:center;background:#fff;padding:44px 60px;border-bottom:1px solid #eee;font-size:48px}
.rv{color:#999;font-size:42px}.rd{font-size:36px;color:#999;margin-top:10px}
.tg{width:150px;height:90px;border-radius:45px;background:#E5E5EA;position:relative;flex:none}.tg i{position:absolute;left:6px;top:6px;width:78px;height:78px;border-radius:50%;background:#fff;box-shadow:0 3px 8px rgba(0,0,0,.2)}
.tg.on{background:#34C759}.tg.on i{left:66px}
/* 新闻 */
.news{display:flex;gap:30px;padding:44px 50px;border-bottom:1px solid #eee}.nt{flex:1}.nh{font-size:52px;line-height:1.45}
.nm{font-size:34px;color:#999;margin-top:22px}.nimg{width:340px;height:240px;border-radius:12px;overflow:hidden;flex:none}
/* 菜谱 */
.rhero{height:900px;position:relative;color:#fff}.rpic{position:absolute;inset:0;opacity:.6}.rht{position:absolute;left:60px;bottom:60px}
.rht h1{font-size:90px;margin:0 0 20px}.rht div{font-size:46px}
.sec{padding:20px 60px}.ingr{display:flex;justify-content:space-between;font-size:46px;padding:26px 0;border-bottom:1px dashed #ddd}
.step{padding:30px 60px}.sn{font-size:44px;font-weight:bold;color:#E65100}.step p{font-size:48px;line-height:1.75;margin:20px 0}
.spic{height:600px;border-radius:16px;overflow:hidden}
.tip{margin:30px 60px;padding:40px;background:#FFF8E1;border-radius:20px;font-size:44px;line-height:1.7}.tip b{display:block;color:#F57F17;margin-bottom:10px}
.cmt{display:flex;gap:30px;padding:36px 60px;border-bottom:1px solid #f0f0f0;font-size:44px;line-height:1.6}
.cav{width:100px;height:100px;border-radius:50%;color:#fff;display:flex;align-items:center;justify-content:center;flex:none}.cn{color:#888;font-size:38px}
/* 攻略 */
.k-travel .rhero{margin:0 -64px 40px}.k-travel .tip{margin:0 0 46px;background:#E3F2FD}.k-travel .tip b{color:#1565C0}
.dayb{display:inline-block;background:#00897B;color:#fff;font-size:46px;font-weight:bold;padding:16px 40px;border-radius:40px;margin:30px 0 40px}
.spot{display:flex;gap:30px;margin-bottom:20px}.sp{font-size:54px;font-weight:bold;margin-bottom:16px}
/* 终端与代码 */
.k-term #page{background:#1E1E1E;color:#D4D4D4}.k-code #page{background:#FFFFFF}
.win{height:44px;background:#323233;display:flex;align-items:center;padding:0 16px;gap:8px;color:#ccc;font-size:14px}
.win i{width:13px;height:13px;border-radius:50%;display:inline-block}.win span{margin-left:auto;margin-right:auto}
.k-term #c{padding:12px 20px 30px;font-family:Consolas,"Microsoft YaHei",monospace;font-size:17px;line-height:1.55}
.ln{white-space:pre-wrap}.pr{color:#6A9955}.ok{color:#89D185}.err{color:#F48771}.warn{color:#DCDCAA}
.k-code .win{background:#F3F3F3;color:#333;border-bottom:1px solid #ddd}.k-code #c{counter-reset:ln;padding:16px 0 30px}
.code{white-space:pre;font-family:Consolas,monospace;font-size:17px;line-height:1.6;padding:0 0 18px 70px;color:#1F1F1F;position:relative}
/* 表格 */
.k-table #page{background:#F0F2F5}.nav{height:56px;background:#001529;color:#fff;display:flex;align-items:center;padding:0 24px;font-size:18px;gap:40px}
.nav b{font-size:20px}.ttl{padding:20px 24px 8px;font-size:22px;font-weight:bold}
.filter{margin:8px 24px;background:#fff;padding:14px;font-size:14px;color:#888;border-radius:4px}
.k-table #c{margin:0 24px 24px;background:#fff;border-radius:4px}
.tb{width:100%;border-collapse:collapse;table-layout:fixed;font-size:15px}.tb td{padding:11px 16px;border-bottom:1px solid #f0f0f0}
.tb.head td{background:#FAFAFA;font-weight:bold;color:#333}
.badge{display:inline-block;padding:1px 8px;border-radius:4px;background:#F6FFED;color:#389E0D;border:1px solid #B7EB8F;font-size:13px}
.badge.bad{background:#FFF1F0;color:#CF1322;border-color:#FFA39E}.pager{padding:16px 24px 24px;text-align:right;font-size:14px;color:#666}
/* 朋友圈 */
.cover{height:820px;position:relative;margin-bottom:80px}.cover .pic{height:820px;border-radius:0}
.cover .me{position:absolute;right:50px;bottom:-50px;display:flex;align-items:flex-end;gap:30px;font-size:50px;font-weight:bold;color:#fff}
.cover .av{width:200px;height:200px;font-size:80px}
.post{display:flex;gap:30px;padding:44px 40px;border-bottom:1px solid #eee}.pav{width:120px;height:120px;border-radius:12px;color:#fff;font-size:52px;display:flex;align-items:center;justify-content:center;flex:none}
.pb{flex:1}.pn{color:#576B95;font-size:46px;font-weight:bold}.pt{font-size:48px;line-height:1.55;margin:12px 0 20px}
.grid{display:grid;gap:10px;margin-bottom:20px}.g1{grid-template-columns:560px}.g1 .gimg{height:560px}
.g2{grid-template-columns:repeat(2,250px)}.g3{grid-template-columns:repeat(3,250px)}.gimg{height:250px;overflow:hidden}
.pm{display:flex;justify-content:space-between;font-size:36px;color:#999}.dots{background:#F2F2F2;color:#576B95;padding:0 22px;border-radius:8px}
.lbox{background:#F3F3F5;margin-top:20px;padding:16px 24px;font-size:40px;line-height:1.6}.lk{color:#576B95}.cm b{color:#576B95;font-weight:normal}
'''


def chrome(page):
    """页面顶部与底部（不含开头块）"""
    k, w = page['kind'], page['w']
    if k in ('term', 'code'):
        top = (f'<div class="win"><i style="background:#FF5F57"></i><i style="background:#FEBC2E"></i>'
               f'<i style="background:#28C840"></i><span>{esc(page["bar"])}</span></div>')
        return top, ''
    if k == 'table':
        top = (f'<div class="nav"><b>◆ 运营后台</b><span>首页</span><span>数据</span><span>订单</span><span>设置</span></div>'
               f'<div class="ttl">{esc(page["bar"])}</div><div class="filter">筛选：全部　　时间：近 30 天　　关键词：</div>')
        return top, '<div class="pager">共 1,286 条　〈 1 2 3 4 5 … 65 〉　20 条/页</div>'
    status = STATUS if w <= 1080 else ''
    bar = f'<div class="bar"><span class="back">‹</span>{esc(page["bar"])}<span class="more">···</span></div>'
    if k == 'moments':
        bar = '' if page.get('normal') else bar
        cover = f'<div class="cover">{picture(page["me"])}<div class="me">{esc(page["me"])}{avatar(page["me"])}</div></div>'
        return status + bar + cover, '<div class="end">— 没有更多了 —</div>'
    if k == 'chat':
        return status + bar, '<div class="inputbar"><div class="ci"></div><div class="in"></div><div class="ci"></div><div class="ci"></div></div>'
    if k == 'product':
        return status, '<div class="buybar"><span class="a">加入购物车</span><span class="b">立即购买</span></div>'
    if k == 'article':
        return status + bar, '<div class="afoot"><span>阅读 10万+</span><span>♡ 赞　　在看</span></div>'
    if k == 'news':
        tabs = block(('ntabs', ['推荐', '本地', '科技', '体育', '财经', '热榜'], page['bar']), page)
        return status + tabs, '<div class="end">— 已经到底了 —</div>'
    return status + bar, '<div class="end">— 已经到底了 —</div>'


JS = '''
const P = JSON.parse(document.getElementById('data').textContent)
const c = document.getElementById('c'), page = document.getElementById('page')
const add = (h) => { const t = document.createElement('template'); t.innerHTML = h; const el = t.content.firstElementChild; c.appendChild(el); return el }
for (const h of P.head) add(h)
let fi = 0, di = 0, miss = 0
const marks = []
while (true) {
  const h = page.offsetHeight
  if (di < P.det.length && h >= P.det[di][0] * P.target) { marks.push(add(P.det[di++][1])); continue }
  if (h >= P.target || fi >= P.fill.length) break
  const el = add(P.fill[fi++])
  // 普通截图不能超过长宽比上限：放不下的块撤掉，换后面的块再试
  if (P.max && page.offsetHeight > P.max) { el.remove(); if (++miss > 50) break }
}
c.querySelectorAll('.num').forEach((e, i) => { e.textContent = i + 1 })
const y0 = page.getBoundingClientRect().top
document.documentElement.setAttribute('data-r', JSON.stringify({h: page.offsetHeight,
  d: marks.map((e) => { const r = e.getBoundingClientRect(); return [Math.round(r.top - y0), Math.round(r.bottom - y0)] })}))
'''


def page_html(page, r):
    w = page['w']
    if page.get('normal'):
        target, maxh = round(w * 2.1), round(w * 2.45)
    else:
        target, maxh = page['h'], 0
    top, bottom = chrome(page)
    head = [block(b, page) for b in page.get('head', [])]
    if page['kind'] == 'table':
        head.insert(0, f'<table class="tb head"><tr>{"".join(f"<td>{esc(x)}</td>" for x in page["cols"])}</tr></table>')
    data = {
        'target': target, 'max': maxh, 'head': head,
        'fill': [block(b, page) for b in fill(page, r)],
        'det': [[d['at'], block(d['b'], page)] for d in page.get('det', [])],
    }
    cls = f'k-{page["kind"]} s-{page.get("style", "")}'
    return ('<!doctype html><html><head><meta charset="utf-8"><style>' + CSS.replace('WIDTH', str(w)) + '</style></head>'
            f'<body class="{cls}"><div id="page">{top}<div id="c"></div>{bottom}</div>'
            '<script id="data" type="application/json">' + json.dumps(data, ensure_ascii=False).replace('</', '<\\/') + '</script>'
            f'<script>{JS}</script></body></html>')


def edge(profile, *args):
    return subprocess.run([EDGE, '--headless=new', '--disable-gpu', '--hide-scrollbars', '--force-device-scale-factor=1',
                           '--no-first-run', '--disable-extensions', f'--user-data-dir={profile}', *args],
                          capture_output=True, timeout=120)


def render(job):
    """两遍 Edge：先取布局，再按整页高度截图。返回 (高度, 细节区间)"""
    src, png, w, profile = job
    url = src.resolve().as_uri()
    out = edge(profile, f'--window-size={w},1000', '--dump-dom', url).stdout.decode('utf-8', 'replace')
    m = re.search(r'data-r="([^"]*)"', out)
    if not m:
        raise RuntimeError(f'Edge 没有输出布局：{src}')
    lay = json.loads(html.unescape(m.group(1)))
    edge(profile, f'--window-size={w},{lay["h"]}', f'--screenshot={png}', url)
    if not png.exists():
        raise RuntimeError(f'Edge 没有生成截图：{png}')
    return lay


def png_size(path):
    with open(path, 'rb') as f:
        head = f.read(24)
    return int.from_bytes(head[16:20], 'big'), int.from_bytes(head[20:24], 'big')


def build(root):
    r = rng(NAME)
    pages = [dict(p, w=p.get('w', 1080)) for p in L.PAGES]
    order = list(range(len(pages)))
    r.shuffle(order)
    out = reset_dir(root / 'files' / NAME)
    tmp = Path(tempfile.mkdtemp(prefix='rummage-long-'))
    jobs = []
    for num, idx in enumerate(order, 1):
        p = pages[idx]
        p['file'] = f'long_{num:03d}.png'
        src = tmp / f'{num:03d}.html'
        # 每页独立的种子，填充内容不随页面顺序变化
        src.write_text(page_html(p, rng(f'{NAME}-{idx}')), encoding='utf-8')
        jobs.append((src, out / p['file'], p['w'], tmp / f'profile-{num % 4}'))
    # 4 个 Edge 同时运行，每个用自己的配置目录（同一配置目录不能被两个实例同时使用）
    lays = []
    for k in range(0, len(jobs), 4):
        with ThreadPoolExecutor(4) as ex:
            lays += list(ex.map(render, jobs[k:k + 4]))
    shutil.rmtree(tmp, ignore_errors=True)
    for (_, png, w, _), idx, lay in zip(jobs, order, lays):
        size = png_size(png)
        if size != (w, lay['h']):
            raise RuntimeError(f'{png.name} 的尺寸 {size} 与页面 {w}×{lay["h"]} 不符')
        pages[idx]['size'] = size
        pages[idx]['regions'] = lay['d']

    qs = []
    for idx, p in enumerate(pages):
        file = f'files/{NAME}/{p["file"]}'
        if p.get('q'):
            qs.append({'id': f'{NAME}-{idx:03d}-0', 'subset': NAME, 'query': p['q'], 'relevant': [{'file': file}]})
        for j, (d, reg) in enumerate(zip(p.get('det', []), p['regions']), 1):
            if d['q']:
                qs.append({'id': f'{NAME}-{idx:03d}-{j}', 'subset': NAME, 'query': d['q'],
                           'relevant': [{'file': file, 'region': reg}]})

    ratios = sorted(p['size'][1] / p['size'][0] for p in pages)
    meta = {
        'name': NAME,
        'modality': 'image',
        'description': '自制的手机与网页竖向长截屏（聊天、文章、商品详情、账单、设置、新闻、菜谱、攻略、终端、表格、朋友圈等），'
                       '查询多数指向截图中下部的细节，考查长图切片；另含 10 张长宽比 2.0–2.5 的普通截图作干扰项。',
        'source': '由仓库 scripts/eval/build_long.py 生成：页面文本自行撰写（long_pages.py），HTML + CSS 排版，Edge 无头模式截整页 PNG',
        'license': '自制内容，可自由使用（人名、店名、品牌均为虚构）。',
        'query_lang': 'zh',
        'query_origin': 'llm',
        'count_files': len(pages),
        'count_queries': len(qs),
        'notes': f'长宽比 {ratios[0]:.2f}–{ratios[-1]:.2f}；细节查询的相关项带 region（细节在图中的纵向像素区间 [上, 下]）',
    }
    return meta, qs

