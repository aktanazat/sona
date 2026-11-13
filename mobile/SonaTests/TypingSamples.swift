/// Text typed on the Sona keyboard, each ending in the word that space would correct.
/// `typos` and `keeps` were written before the corrector first ran; its weights were then
/// tuned on their failures. `freshTypos` and `freshKeeps` were written after that tuning.
enum TypingSamples {
    struct Typo {
        let text: String
        let intended: String
    }

    static let lexicon = TypingLexicon(
        replacements: [
            ("Aktan Azat", "Aktan Azat"), ("Nolan", "Nolan"), ("Akyl", "Akyl"), ("Dauren", "Dauren"),
            ("omw", "On my way!"),
        ],
        vocabulary: ["Sona", "Herdr", "engram", "Tauri", "omp"]
    )

    static let typos: [Typo] = [
        ("I was woth", "with"), ("that sounds goid", "good"), ("see you nexr", "next"),
        ("what is it abour", "about"), ("thank yoy", "you"), ("wjat", "what"), ("a note frim", "from"),
        ("I am hwre", "here"), ("what tume", "time"), ("some peoole", "people"), ("have you beeb", "been"),
        ("I woukd", "would"), ("we coukd", "could"), ("I rhink", "think"), ("I don't kniw", "know"),
        ("try agaon", "again"), ("are you free todsy", "today"), ("good nighy", "night"),
        ("see you at dinnet", "dinner"), ("good morninf", "morning"), ("this evenimg", "evening"),
        ("check the calemdar", "calendar"), ("send the schedulr", "schedule"), ("any updste", "update"),
        ("the shared documemt", "document"), ("over budgrt", "budget"), ("the last invoive", "invoice"),
        ("this weekenf", "weekend"), ("grab lumch", "lunch"), ("so happt", "happy"),
        ("for her birthsay", "birthday"), ("can you chrck", "check"), ("one anothrr", "another"),
        ("five minutrs", "minutes"), ("so sorrt", "sorry"), ("lwt", "let"), ("can you gwt", "get"),
        ("that is verh", "very"), ("you are rignt", "right"), ("thanks so mich", "much"),
        ("is thsi", "this"), ("my freind", "friend"), ("still wroking", "working"), ("a big chnage", "change"),
        ("no probelm", "problem"), ("the anwser", "answer"), ("me aslo", "also"), ("I am sitll", "still"),
        ("the otehr", "other"), ("let me know wehn", "when"), ("hwo", "how"), ("is that yuor", "your"),
        ("where are you gonig", "going"), ("you cna", "can"), ("the smae", "same"), ("one mroe", "more"),
        ("I liek", "like"), ("do you wnat", "want"), ("the nwe", "new"), ("the fisrt", "first"),
        ("the olny", "only"), ("one thnig", "thing"), ("we shoudl", "should"), ("my buisness", "business"),
        ("we can acheive", "achieve"),
        ("my adress", "address"), ("from the begining", "beginning"), ("the enviroment", "environment"),
        ("I remeber", "remember"), ("call me imediately", "immediately"), ("I recomend", "recommend"),
        ("we can acomodate", "accommodate"), ("it occured", "occurred"), ("finaly", "finally"),
        ("very intresting", "interesting"), ("something diffrent", "different"), ("looks familar", "familiar"),
        ("what hapened", "happened"), ("so beautful", "beautiful"), ("this is importnt", "important"),
        ("a quick questin", "question"), ("a new studnt", "student"), ("more cofee", "coffee"),
        ("helo", "hello"), ("a short spech", "speech"),
        ("I am comming", "coming"), ("still writting", "writing"), ("verry", "very"),
        ("I allready", "already"), ("be carefull", "careful"), ("it was successfull", "successful"),
        ("walk accross", "across"), ("a special occassion", "occasion"), ("if neccessary", "necessary"),
        ("too agressive", "aggressive"), ("so beautifull", "beautiful"), ("so goood", "good"),
        ("give me a callk", "call"), ("I will sendf", "send"), ("I am free toaday", "today"),
        ("on my way homee", "home"),
        ("the team calender", "calendar"), ("an independant", "independent"), ("its existance", "existence"),
        ("barely noticable", "noticeable"), ("said it publically", "publicly"), ("apparantly", "apparently"),
        ("the rythm", "rhythm"), ("bit my tounge", "tongue"), ("I want to persue", "pursue"),
        ("such a sucess", "success"), ("are you concious", "conscious"), ("a new resturant", "restaurant"),
        ("yours truely", "truly"), ("a long arguement", "argument"), ("so embarass", "embarrass"),
        ("a small tatoo", "tattoo"), ("I can't beleive", "believe"), ("see you wensday", "Wednesday"),
        ("in febuary", "February"), ("at the libary", "library"),
        ("I didnt", "didn't"), ("it isnt", "isn't"), ("it doesnt", "doesn't"), ("I wouldnt", "wouldn't"),
        ("youre", "you're"), ("thats", "that's"), ("ive", "I've"),
        ("atleast", "at least"), ("help eachother", "each other"), ("right infront", "in front"),
        ("thankyou", "thank you"),
        ("Waht", "What"), ("Done. Yuo", "You"), ("Ok! Cna", "Can"), ("Wehn", "When"), ("Sure.\nHwo", "How"),
    ].map { Typo(text: $0.0, intended: $0.1) }

    /// Each ends in a word space must leave exactly as typed.
    static let keeps: [String] = [
        "see you tomorrow", "the meeting", "thanks", "okay", "definitely", "receive", "separate", "I think so",
        "lol", "btw", "omg", "gonna", "yeah", "idk", "tbh",
        "Aktan", "with Azat", "ask Nolan", "Akyl", "Dauren", "I met Zhanel",
        "Sona", "Herdr", "an engram", "Tauri", "omp", "omw",
        "meet at 10am", "the 3rd", "v2", "call 555-0134",
        "https://example.com/teh", "www.githbu.com", "aktan@gmial.com", "example.co",
        "NASA", "ASAP", "TEH", "FYI", "my iPhone", "on GitHub", "macOS",
    ]

    static let freshTypos: [Typo] = [
        ("my huose", "house"), ("some wster", "water"), ("call my phine", "phone"), ("save some monry", "money"),
        ("my famiky", "family"), ("after schoool", "school"), ("at the offive", "office"),
        ("one minite", "minute"), ("the phone numbet", "number"), ("send a pictire", "picture"),
        ("we love to travek", "travel"), ("my flihgt", "flight"), ("the hotle", "hotel"),
        ("at the airpot", "airport"), ("a new tickte", "ticket"), ("see the doctot", "doctor"),
        ("my appointmnet", "appointment"), ("order pizzza", "pizza"), ("watch a moive", "movie"),
        ("good musci", "music"), ("the weathet", "weather"), ("this summet", "summer"),
        ("last wimter", "winter"), ("mabye", "maybe"), ("that soumds", "sounds"), ("that is awsome", "awesome"),
        ("sounds perfrct", "perfect"), ("so excitec", "excited"), ("I am so tirwd", "tired"),
        ("very buzy", "busy"), ("somewhere aroudn", "around"), ("in betwen", "between"),
        ("even thoguh", "though"), ("go throught", "through"), ("I thougt", "thought"),
        ("good enoguh", "enough"), ("hi everyome", "everyone"), ("is there anytihng", "anything"),
        ("there is notihng", "nothing"), ("thanks for everyting", "everything"), ("very eatly", "early"),
        ("see you latet", "later"), ("see you sooon", "soon"), ("yesterdya", "yesterday"),
        ("are you free tonigth", "tonight"), ("next mnoth", "month"), ("this yeat", "year"),
        ("on fridya", "Friday"), ("on saturady", "Saturday"), ("send a phot", "photo"),
        ("a short vidoe", "video"), ("I will replu", "reply"), ("good ifea", "idea"),
        ("what is the paln", "plan"), ("let's do it togehter", "together"), ("I actualy", "actually"),
        ("honesly", "honestly"), ("I usualy", "usually"), ("we are almsot", "almost"), ("I alwasy", "always"),
        ("I nevr", "never"), ("couldnt", "couldn't"), ("shouldnt", "shouldn't"), ("theyre", "they're"),
        ("wasnt", "wasn't"), ("Tahnks", "Thanks"), ("See you. Mabye", "Maybe"), ("ofcourse", "of course"),
        ("noone", "no one"), ("Hpw", "How"),
    ].map { Typo(text: $0.0, intended: $0.1) }

    static let freshKeeps: [String] = [
        "see you at the airport", "my phone", "awesome", "honestly", "pizza",
        "brb", "ttyl", "haha", "hmm", "ya",
        "with Bekzat", "ping Akyl", "the Sona",
        "at 2pm", "in 4K", "Q3", "user_id", "#launch", "@aktan", "sona.app", "e.g.", "PDF", "XL", "OK",
    ]
}
