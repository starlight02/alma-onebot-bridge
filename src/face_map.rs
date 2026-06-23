//! QQ face expression ID → human-readable name mapping.
//!
//! Covers the most commonly used QQ "super expressions" (超级表情).
//! Source: go-onebot-agent / NapCat face table.
//! IDs not in this table fall back to `[emoji:face_X]`.

/// Look up a QQ face expression name by its numeric ID.
/// Returns `None` for unknown IDs.
pub fn face_name(id: &str) -> Option<&'static str> {
    match id {
        "0" => Some("惊讶"),
        "1" => Some("撇嘴"),
        "2" => Some("色"),
        "3" => Some("发呆"),
        "4" => Some("得意"),
        "5" => Some("流泪"),
        "6" => Some("害羞"),
        "7" => Some("闭嘴"),
        "8" => Some("睡"),
        "9" => Some("大哭"),
        "10" => Some("尴尬"),
        "11" => Some("发怒"),
        "12" => Some("调皮"),
        "13" => Some("呲牙"),
        "14" => Some("微笑"),
        "15" => Some("难过"),
        "16" => Some("酷"),
        "18" => Some("抓狂"),
        "19" => Some("吐"),
        "20" => Some("偷笑"),
        "21" => Some("可爱"),
        "22" => Some("白眼"),
        "23" => Some("傲慢"),
        "24" => Some("饥饿"),
        "25" => Some("困"),
        "26" => Some("惊恐"),
        "27" => Some("流汗"),
        "28" => Some("憨笑"),
        "29" => Some("悠闲"),
        "30" => Some("奋斗"),
        "31" => Some("咒骂"),
        "32" => Some("疑问"),
        "33" => Some("嘘"),
        "34" => Some("晕"),
        "35" => Some("折磨"),
        "36" => Some("衰"),
        "37" => Some("骷髅"),
        "38" => Some("敲打"),
        "39" => Some("再见"),
        "46" => Some("猪头"),
        "49" => Some("拥抱"),
        "53" => Some("蛋糕"),
        "59" => Some("便便"),
        "60" => Some("咖啡"),
        "63" => Some("玫瑰"),
        "66" => Some("爱心"),
        "74" => Some("太阳"),
        "75" => Some("月亮"),
        "76" => Some("赞"),
        "78" => Some("握手"),
        "79" => Some("胜利"),
        "85" => Some("飞吻"),
        "96" => Some("冷汗"),
        "97" => Some("擦汗"),
        "98" => Some("抠鼻"),
        "99" => Some("鼓掌"),
        "100" => Some("糗大了"),
        "101" => Some("坏笑"),
        "102" => Some("左哼哼"),
        "103" => Some("右哼哼"),
        "104" => Some("哈欠"),
        "106" => Some("委屈"),
        "109" => Some("左亲亲"),
        "111" => Some("可怜"),
        "112" => Some("菜刀"),
        "113" => Some("啤酒"),
        "116" => Some("示爱"),
        "118" => Some("抱拳"),
        "120" => Some("拳头"),
        "122" => Some("爱你"),
        "123" => Some("NO"),
        "124" => Some("OK"),
        "125" => Some("转圈"),
        "129" => Some("挥手"),
        "144" => Some("喝彩"),
        "147" => Some("棒棒糖"),
        "171" => Some("茶"),
        "173" => Some("泪奔"),
        "174" => Some("无奈"),
        "175" => Some("卖萌"),
        "176" => Some("小纠结"),
        "178" => Some("斜眼笑"),
        "179" => Some("doge"),
        "180" => Some("惊喜"),
        "181" => Some("哈哈"),
        "182" => Some("笑哭"),
        "183" => Some("面无表情"),
        "201" => Some("点赞"),
        "203" => Some("拜托"),
        "205" => Some("送花"),
        "212" => Some("托腮"),
        "214" => Some("啵啵"),
        "219" => Some("抱抱"),
        "222" => Some("摸头"),
        "262" => Some("不看"),
        "264" => Some("捂脸"),
        "265" => Some("辣眼睛"),
        "271" => Some("吃瓜"),
        "272" => Some("呵呵"),
        "273" => Some("黑脸"),
        "277" => Some("加油"),
        "281" => Some("沉默"),
        "282" => Some("笑眼"),
        "287" => Some("白眼"),
        "289" => Some("亲亲"),
        "290" => Some("开心"),
        "293" => Some("期待"),
        "294" => Some("捂嘴笑"),
        "305" => Some("脑阔疼"),
        "306" => Some("沧桑"),
        "311" => Some("打call"),
        "312" => Some("变形"),
        "314" => Some("暗中观察"),
        "317" => Some("问号脸"),
        "318" => Some("嘿哈"),
        "319" => Some("捂脸"),
        "320" => Some("社会社会"),
        "322" => Some("我的天"),
        "326" => Some("喷脸"),
        "327" => Some("打脸"),
        "336" => Some("汪汪"),
        "337" => Some("喵喵"),
        "338" => Some("牛气冲天"),
        "340" => Some("无眼笑"),
        "341" => Some("敬礼"),
        "343" => Some("面无表情"),
        "344" => Some("摸鱼"),
        "345" => Some("哦"),
        "346" => Some("请"),
        "347" => Some("拜拜"),
        "349" => Some("坚强"),
        "350" => Some("贴贴"),
        "351" => Some("敲敲"),
        "352" => Some("咦"),
        "353" => Some("拜托"),
        "354" => Some("尊嘟假嘟"),
        "355" => Some("耶"),
        "356" => Some("666"),
        "357" => Some("裂开"),
        _ => None,
    }
}

/// Reverse lookup: find a QQ face expression ID by its human-readable name.
/// Returns `None` for unknown names.
pub fn face_id(name: &str) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::OnceLock;

    static REVERSE: OnceLock<HashMap<&'static str, u16>> = OnceLock::new();

    let map = REVERSE.get_or_init(|| {
        let mut m = HashMap::with_capacity(200);
        // Scan the full known ID range; face_name returns &'static str
        for id in 0u16..=500 {
            let id_str = id.to_string();
            if let Some(n) = face_name(&id_str) {
                m.entry(n).or_insert(id);
            }
        }
        m
    });

    map.get(name).map(u16::to_string)
}

#[cfg(test)]
mod tests {
    use super::{face_id, face_name};

    #[test]
    fn duplicate_face_names_keep_first_known_id() {
        assert_eq!(face_name("264"), Some("捂脸"));
        assert_eq!(face_name("319"), Some("捂脸"));
        assert_eq!(face_id("捂脸"), Some("264".to_string()));
    }
}
